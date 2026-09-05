pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

pub fn pack_rgba(
    raw: &[u8],
    width: usize,
    height: usize,
    align: usize,
    bgra: bool,
) -> Result<Vec<u8>, String> {
    let row = width.checked_mul(4).ok_or("Frame width overflow")?;
    let size = row.checked_mul(height).ok_or("Frame size overflow")?;
    let align = align.max(1);
    if width == 0 || height == 0 || size > MAX_FRAME_BYTES || !align.is_power_of_two() {
        return Err("Invalid frame dimensions or alignment".into());
    }
    let stride = row.checked_add(align - 1).ok_or("Stride overflow")? & !(align - 1);
    let required = stride
        .checked_mul(height - 1)
        .and_then(|n| n.checked_add(row))
        .ok_or("Stride overflow")?;
    if raw.len() < required {
        return Err("Incomplete frame".into());
    }
    let mut packed = Vec::with_capacity(size);
    for y in 0..height {
        packed.extend_from_slice(&raw[y * stride..y * stride + row]);
    }
    if bgra {
        for p in packed.chunks_exact_mut(4) {
            p.swap(0, 2);
        }
    }
    Ok(packed)
}

pub fn remote_point(
    origin: (i32, i32),
    size: (i32, i32),
    point: (i32, i32),
) -> Result<(i32, i32), String> {
    if point.0 < 0 || point.1 < 0 || point.0 >= size.0 || point.1 >= size.1 {
        return Err("Coordinates are outside the selected display".into());
    }
    Ok((
        origin
            .0
            .checked_add(point.0)
            .ok_or("X coordinate overflow")?,
        origin
            .1
            .checked_add(point.1)
            .ok_or("Y coordinate overflow")?,
    ))
}
