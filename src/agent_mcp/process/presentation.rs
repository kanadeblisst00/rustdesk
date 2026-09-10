use serde_json::{json, Value};

const NS: &str = "http://schemas.microsoft.com/powershell/2004/04";

fn bounded(text: &str, limit: usize) -> (&str, bool) {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], end < text.len())
}

fn plain(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut result = String::new();
    let (mut copied, mut i) = (0, 0);
    while i + 1 < bytes.len() {
        if bytes[i] != 0x1b {
            i += 1;
            continue;
        }
        let end = match bytes[i + 1] {
            b'[' => {
                let mut j = i + 2;
                while j < bytes.len() && (0x30..=0x3f).contains(&bytes[j]) {
                    j += 1;
                }
                while j < bytes.len() && (0x20..=0x2f).contains(&bytes[j]) {
                    j += 1;
                }
                (j < bytes.len() && (0x40..=0x7e).contains(&bytes[j])).then_some(j + 1)
            }
            b']' => {
                let mut j = i + 2;
                let mut end = None;
                while j < bytes.len() {
                    if bytes[j] == 7 {
                        end = Some(j + 1);
                        break;
                    }
                    if bytes[j..].starts_with(b"\x1b\\") {
                        end = Some(j + 2);
                        break;
                    }
                    j += 1;
                }
                end
            }
            _ => None,
        };
        if let Some(end) = end {
            result.push_str(&text[copied..i]);
            copied = end;
            i = end;
        } else if bytes[i + 1] == b']' {
            break;
        } else {
            i += 1;
        }
    }
    result.push_str(&text[copied..]);
    result
}

fn unescape(mut text: &str) -> Result<String, String> {
    let mut units = Vec::new();
    while !text.is_empty() {
        let bytes = text.as_bytes();
        if bytes.len() >= 7
            && bytes.starts_with(b"_x")
            && bytes[6] == b'_'
            && bytes[2..6].iter().all(u8::is_ascii_hexdigit)
        {
            units.push(u16::from_str_radix(&text[2..6], 16).map_err(|e| e.to_string())?);
            text = &text[7..];
        } else {
            let character = text.chars().next().ok_or("Missing CLIXML character")?;
            units.extend_from_slice(character.encode_utf16(&mut [0; 2]));
            text = &text[character.len_utf8()..];
        }
    }
    String::from_utf16(&units).map_err(|_| "Invalid CLIXML UTF-16 escape sequence".into())
}

fn marker(text: &str) -> Option<usize> {
    text.match_indices("#< CLIXML")
        .find(|(i, _)| *i == 0 || text.as_bytes()[i - 1] == b'\n')
        .map(|(i, _)| i)
}

fn clixml(mut text: &str) -> Result<Value, String> {
    let mut display = String::new();
    let mut records = Vec::new();
    let mut filtered = 0;
    let mut total = 0;
    let mut truncated = false;
    while let Some(start) = marker(text) {
        display.push_str(&text[..start]);
        let xml = text[start + "#< CLIXML".len()..].trim_start();
        let end = xml.find("</Objs>").ok_or("Incomplete CLIXML document")? + "</Objs>".len();
        let xml_part = &xml[..end];
        if xml_part.contains("<!DOCTYPE") {
            return Err("CLIXML DTD is not supported".into());
        }
        let document = roxmltree::Document::parse_with_options(
            xml_part,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: 4096,
            },
        )
        .map_err(|e| format!("Invalid or incomplete CLIXML: {e}"))?;
        let root = document.root_element();
        if !root.has_tag_name((NS, "Objs")) {
            return Err("Unexpected CLIXML root or namespace".into());
        }
        if root
            .children()
            .any(|n| n.is_text() && !n.text().unwrap_or("").trim().is_empty())
        {
            return Err("Unexpected text between CLIXML records".into());
        }
        for node in root.children().filter(roxmltree::Node::is_element) {
            total += 1;
            let stream = node.attribute("S").unwrap_or("output").to_ascii_lowercase();
            let value = if node.has_tag_name((NS, "S")) && !node.children().any(|n| n.is_element())
            {
                Some(node.text().unwrap_or(""))
            } else if node.has_tag_name((NS, "Obj")) {
                node.children()
                    .find(|n| {
                        n.has_tag_name((NS, "ToString")) && !n.children().any(|c| c.is_element())
                    })
                    .and_then(|n| n.text())
            } else {
                None
            };
            let value = match value {
                Some(value) => unescape(value)?,
                None => xml_part[node.range()].to_owned(),
            };
            if stream == "progress" && node.tag_name().namespace() == Some(NS) {
                filtered += 1;
            } else {
                display.push_str(&value);
                if !value.ends_with('\n') {
                    display.push('\n');
                }
            }
            if records.len() < 16 {
                let value = plain(&value);
                let (value, shortened) = bounded(&value, 256);
                let (stream, short_stream) = bounded(&stream, 64);
                truncated |= shortened || short_stream;
                records.push(
                    json!({"stream":stream,"text":value,"truncated":shortened || short_stream}),
                );
            } else {
                truncated = true;
            }
        }
        text = &xml[end..];
    }
    display.push_str(text);
    let display = plain(&display);
    let (display, shortened) = bounded(&display, 4096);
    Ok(
        json!({"format":"powershell_clixml","text":display,"truncated":truncated || shortened,
        "records":records,"records_total":total,"progress_records_filtered":filtered,"error":null}),
    )
}

pub(super) fn annotate(result: &mut Value) {
    let Some(text) = result["text"].as_str() else {
        return;
    };
    let text = text.trim_start_matches('\u{feff}');
    if marker(text).is_some() {
        result["presentation"] = match clixml(text) {
            Ok(value) => value,
            Err(error) => json!({"format":"powershell_clixml","text":null,"error":error,
                "recovery":"Use original text/data_base64; join adjacent byte chunks before parsing. No partial XML was discarded."}),
        };
    } else if text.contains('\x1b') {
        let clean = plain(text);
        let (clean, truncated) = bounded(&clean, 4096);
        result["presentation"] =
            json!({"format":"ansi","text":clean,"truncated":truncated,"error":null});
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(body: &str) -> String {
        format!("#< CLIXML\r\n<Objs Version=\"1.1.0.1\" xmlns=\"{NS}\">{body}</Objs>")
    }

    #[test]
    fn filters_progress_but_keeps_errors_native_text_and_raw_xml() {
        let xml = document(
            r#"<Obj S="progress"><MS><PR N="Record"><AV>Loading</AV></PR></MS></Obj><S S="Error">失败 &amp; _xD83D__xDE00__x000D__x000A_</S><S S="warning">_x005F_x0041_</S>"#,
        );
        let text = format!("native stderr\n{xml}\ntrailing error");
        let mut result = json!({"text":text});
        annotate(&mut result);
        assert_eq!(result["text"], text);
        let p = &result["presentation"];
        assert_eq!(
            p["text"],
            "native stderr\n失败 & 😀\r\n_x0041_\n\ntrailing error"
        );
        assert_eq!(p["progress_records_filtered"], 1);
        assert_eq!(p["records"][1]["stream"], "error");
    }

    #[test]
    fn preserves_unknown_objects_and_handles_multiple_documents() {
        let text = format!(
            "{}\n{}",
            document(r#"<Obj S="Error"><ToString>error detail</ToString></Obj>"#),
            document("<Unknown>important</Unknown>")
        );
        let value = clixml(&text).unwrap();
        assert!(value["text"].as_str().unwrap().contains("error detail"));
        assert!(value["text"]
            .as_str()
            .unwrap()
            .contains("<Unknown>important</Unknown>"));
        assert_eq!(value["records_total"], 2);
    }

    #[test]
    fn incomplete_malformed_or_hostile_xml_never_loses_original_text() {
        for text in [
            "#< CLIXML\n<Objs><S S=\"Error\">split",
            "#< CLIXML\n<!DOCTYPE Objs [<!ENTITY x SYSTEM 'file:///private'>]><Objs>&x;</Objs>",
            "#< CLIXML\n<Objs xmlns=\"wrong\"></Objs>",
            &document("<S>_xD800_</S>"),
        ] {
            let mut result = json!({"text":text});
            annotate(&mut result);
            assert_eq!(result["text"], text);
            assert!(result["presentation"]["text"].is_null());
            assert!(result["presentation"]["error"].is_string());
        }
        let mut raw = json!({"text":null,"encoding":"base64"});
        annotate(&mut raw);
        assert!(raw.get("presentation").is_none());
    }

    #[test]
    fn ansi_cleanup_and_bounded_previews_keep_unicode_and_partial_sequences() {
        let unfinished = "\x1b]".repeat(32768);
        assert_eq!(plain(&unfinished), unfinished);
        assert_eq!(
            plain("\x1b[31m中文\x1b[0m\x1b]0;title\x07 ok\x1b["),
            "中文 ok\x1b["
        );
        assert_eq!(
            plain("\x1b]8;;https://example.invalid\x1b\\link\x1b]8;;\x1b\\"),
            "link"
        );
        let result = clixml(&document(&"<S>中文</S>".repeat(1000))).unwrap();
        assert_eq!(result["records"].as_array().unwrap().len(), 16);
        assert_eq!(result["truncated"], true);
        assert!(result["text"].as_str().unwrap().len() <= 4096);
        assert!(clixml(&document(&"<S>x</S>".repeat(5000))).is_err());
    }
}
