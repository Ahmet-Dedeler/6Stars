use std::fmt::Write as _;

// extract xml tag inner text helper
fn xml_val<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let s = xml.find(&open)? + open.len();
    let e = xml[s..].find(&close)?;
    Some(xml[s..s + e].trim())
}

// summarize xbox gdk MicrosoftGame.Config and layout xml
pub fn summarize_gdk_manifest(game_cfg: &str, layout_xml: Option<&str>) -> String {
    let mut out = String::with_capacity(1024);
    let _ = writeln!(out, "=== Xbox GDK Boot & Package Manifest ===");
    for tag in ["TitleId", "MSAAppId", "StoreId", "SMTEnabled", "XboxLockhartTitleMemory", "XboxAnacondaTitleMemory", "SizeMB"] {
        if let Some(v) = xml_val(game_cfg, tag) {
            let _ = writeln!(out, "  {tag}: {v}");
        }
    }
    if let Some(pos) = game_cfg.find("Executable Name=\"") {
        let rest = &game_cfg[pos + 17..];
        if let Some(end) = rest.find('"') {
            let _ = writeln!(out, "  Executable: {}", &rest[..end]);
        }
    }
    if let Some(lay) = layout_xml {
        let chunks = lay.matches("<Chunk ").count();
        let rpfs = lay.matches(".rpf").count();
        let _ = writeln!(out, "  Layout Chunks: {chunks} | RPF Mount Refs: {rpfs}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gdk_manifest_summary() {
        let cfg = r#"<Game><TitleId>7952987A</TitleId><Executable Name="game_scarlett_master.exe" /><SizeMB>2048</SizeMB></Game>"#;
        let lay = r#"<Package><Chunk Id="1000"><FileGroup Include="common.rpf" /></Chunk></Package>"#;
        let s = summarize_gdk_manifest(cfg, Some(lay));
        assert!(s.contains("TitleId: 7952987A"));
        assert!(s.contains("Executable: game_scarlett_master.exe"));
        assert!(s.contains("Layout Chunks: 1 | RPF Mount Refs: 1"));
    }
}
