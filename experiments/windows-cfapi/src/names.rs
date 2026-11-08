//! Windows filename compatibility, shared by native publication and preflight.
pub fn validate_native_name(name: &str) -> Result<(), String> {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let port = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"));
    if name.is_empty()
        || name.ends_with(['.', ' '])
        || name.encode_utf16().count() > 255
        || name.chars().any(|c| c < ' ' || "<>:\"/\\|?*".contains(c))
        || ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str())
        || port.is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    {
        return Err("unsupported native file name".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_device_aliases_are_rejected_without_touching_a_device() {
        for name in [
            "CON",
            "nul.txt",
            "COM1.log",
            "COM¹",
            "lpt².txt",
            "LPT³",
            "NUL .txt",
            "CONIN$",
            "CONOUT$.txt",
        ] {
            assert!(
                validate_native_name(name).is_err(),
                "accepted device alias {name}"
            );
        }
        for name in [
            "normal.txt",
            "COM0.txt",
            "COM10",
            "comic.txt",
            "中文-😀.docx",
            ".hidden",
            "normal name .txt",
        ] {
            assert!(
                validate_native_name(name).is_ok(),
                "rejected ordinary name {name}"
            );
        }
    }
}
