use serde_json::{json, Value};

pub(super) fn decode(data: &[u8], requested: &str) -> Value {
    if requested == "base64" {
        return json!({"text":null,"encoding":"base64","decoding_error":null});
    }
    if requested == "utf-8" || (requested == "auto" && !cfg!(windows)) {
        match std::str::from_utf8(data) {
            Ok(text) => return json!({"text":text,"encoding":"utf-8","decoding_error":null}),
            Err(_) => {
                return json!({"text":null,"encoding":"utf-8","decoding_error":"Invalid or split UTF-8 sequence; use data_base64 and adjacent chunks, or select the producer's encoding"});
            }
        }
    }
    #[cfg(windows)]
    {
        use windows::Win32::Globalization::GetOEMCP;
        let code_page = if requested == "cp936" {
            936
        } else {
            unsafe { GetOEMCP() }
        };
        let decoded = decode_code_page(data, code_page);
        if requested == "auto" {
            if let Ok(utf8) = std::str::from_utf8(data) {
                if !data.starts_with(b"\xef\xbb\xbf")
                    && decoded.as_deref().is_some_and(|text| text != utf8)
                {
                    return json!({"text":null,"encoding":"ambiguous","encoding_candidates":["utf-8",format!("cp{code_page}")],"decoding_error":"Bytes are valid in both UTF-8 and Windows OEM with different text; explicitly select the producer's encoding"});
                }
                return json!({"text":utf8,"encoding":"utf-8","decoding_error":null});
            }
        }
        if let Some(text) = decoded {
            return json!({"text":text,"encoding":format!("cp{code_page}"),"decoding_error":null});
        }
        return json!({"text":null,"encoding":format!("cp{code_page}"),"decoding_error":"Invalid or split code-page sequence; use data_base64 and adjacent chunks"});
    }
    #[cfg(not(windows))]
    json!({"text":null,"encoding":requested,"decoding_error":"OEM/CP936 decoding requires a Windows peer; use data_base64"})
}

#[cfg(windows)]
fn decode_code_page(data: &[u8], code_page: u32) -> Option<String> {
    use windows::Win32::Globalization::{MultiByteToWideChar, MB_ERR_INVALID_CHARS};
    if data.is_empty() {
        return Some(String::new());
    }
    let size = unsafe { MultiByteToWideChar(code_page, MB_ERR_INVALID_CHARS, data, None) };
    if size <= 0 {
        return None;
    }
    let mut wide = vec![0u16; size as usize];
    let written =
        unsafe { MultiByteToWideChar(code_page, MB_ERR_INVALID_CHARS, data, Some(&mut wide)) };
    if written != size {
        return None;
    }
    String::from_utf16(&wide).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_replaces_invalid_or_split_utf8_with_lossy_text() {
        assert_eq!(decode("中文".as_bytes(), "utf-8")["text"], "中文");
        for data in [&b"\xff"[..], &"中".as_bytes()[..2]] {
            let result = decode(data, "utf-8");
            assert!(result["text"].is_null());
            assert!(result["decoding_error"].is_string());
        }
        assert!(decode(b"abc", "base64")["text"].is_null());
    }

    #[test]
    #[cfg(windows)]
    fn decodes_chinese_windows_output_and_rejects_split_gbk() {
        let result = decode(&[0xcf, 0xb5, 0xcd, 0xb3], "cp936");
        assert_eq!(result["text"], "系统");
        assert_eq!(result["encoding"], "cp936");
        assert!(decode(&[0xcf], "cp936")["text"].is_null());
        // These GBK bytes are also valid UTF-8 ("ϵͳ"); successful UTF-8 parsing is not detection.
        let code_page = unsafe { windows::Win32::Globalization::GetOEMCP() };
        if code_page == 936 {
            assert!(decode(&[0xcf, 0xb5, 0xcd, 0xb3], "auto")["text"].is_null());
        }
    }
}
