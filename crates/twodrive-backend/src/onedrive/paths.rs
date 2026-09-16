use twodrive_core::normalize_cloud_path;

pub(super) fn validate_graph_file_path(path: &str) -> anyhow::Result<()> {
    if path.split('/').any(|name| {
        name.chars()
            .any(|ch| ch.is_control() || "\"*:<>?\\|".contains(ch))
    }) {
        anyhow::bail!(
            "unsupported OneDrive file name in {path}; rename characters \" * : < > ? \\ | before syncing; local content is preserved"
        );
    }
    Ok(())
}

pub(super) fn encode_graph_path(path: &str) -> String {
    normalize_cloud_path(path)
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(percent_encode_path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn split_cloud_parent_name(path: &str) -> anyhow::Result<(String, String)> {
    let path = normalize_cloud_path(path);
    if path == "/" {
        anyhow::bail!("root path does not have a parent/name");
    }
    let (parent, name) = match path.rfind('/') {
        Some(0) => ("/".to_string(), path[1..].to_string()),
        Some(index) => (path[..index].to_string(), path[index + 1..].to_string()),
        None => ("/".to_string(), path),
    };
    if name.is_empty() {
        anyhow::bail!("path does not include a name");
    }
    Ok((parent, name))
}

pub(super) fn graph_parent_lookup_url(parent_path: &str) -> String {
    let parent_path = normalize_cloud_path(parent_path);
    if parent_path == "/" {
        "https://graph.microsoft.com/v1.0/me/drive/root".to_string()
    } else {
        format!(
            "https://graph.microsoft.com/v1.0/me/drive/root:/{}",
            encode_graph_path(&parent_path)
        )
    }
}

pub(super) fn percent_encode_path_segment(segment: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::new();
    for byte in segment.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}
