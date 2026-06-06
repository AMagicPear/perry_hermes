pub fn looks_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(1000)];
    if sample.is_empty() {
        return false;
    }
    let non_printable = sample
        .iter()
        .filter(|b| !matches!(**b, 0x09 | 0x0A | 0x0D | 0x20..=0x7E))
        .count();
    (non_printable * 20) > sample.len()
}
