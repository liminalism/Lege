//! Set `dc:title` / `dc:creator` in an XMP packet.
//!
//! Readers such as Acrobat and calibre prefer XMP over the Info dictionary when
//! both exist, so a document that carries a packet must have it updated too or
//! the new Info values would be shadowed. This is a targeted text edit, not an
//! XML round trip: everything outside the two elements is left byte-for-byte.

const DC_NAMESPACE: &str = "http://purl.org/dc/elements/1.1/";

/// The packet with the given fields replaced or inserted, or `None` if the
/// packet is not UTF-8 or has no `rdf:RDF` element to put them in.
pub fn rewrite(packet: &[u8], title: Option<&str>, author: Option<&str>) -> Option<Vec<u8>> {
    let (bom, body) = match packet.strip_prefix(b"\xEF\xBB\xBF") {
        Some(rest) => (true, rest),
        None => (false, packet),
    };
    let mut text = std::str::from_utf8(body).ok()?.to_string();
    if !text.contains("</rdf:RDF>") {
        return None;
    }
    let mut missing = String::new();
    if let Some(title) = title {
        let element = format!(
            "<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>",
            escape(title)
        );
        if !replace_element(&mut text, "dc:title", &element) {
            missing.push_str(&element);
        }
    }
    if let Some(author) = author {
        let element = format!(
            "<dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>",
            escape(author)
        );
        if !replace_element(&mut text, "dc:creator", &element) {
            missing.push_str(&element);
        }
    }
    if !missing.is_empty() {
        let at = text.rfind("</rdf:RDF>")?;
        text.insert_str(
            at,
            &format!(
                "<rdf:Description rdf:about=\"\" xmlns:dc=\"{DC_NAMESPACE}\">{missing}</rdf:Description>\n"
            ),
        );
    }
    let mut out = Vec::with_capacity(text.len() + 3);
    if bom {
        out.extend_from_slice(b"\xEF\xBB\xBF");
    }
    out.extend_from_slice(text.as_bytes());
    Some(out)
}

/// Replace the first `<name …>…</name>` or `<name …/>` with `element`.
fn replace_element(text: &mut String, name: &str, element: &str) -> bool {
    let open = format!("<{name}");
    let mut search = 0;
    while let Some(found) = text[search..].find(&open) {
        let start = search + found;
        let after_name = start + open.len();
        // `<dc:titleX` is a different element.
        let boundary = text[after_name..].chars().next();
        if !matches!(boundary, Some(ch) if ch == '>' || ch == '/' || ch.is_whitespace()) {
            search = after_name;
            continue;
        }
        let Some(tag_end) = text[start..].find('>').map(|offset| start + offset) else {
            return false;
        };
        let end = if text[..tag_end].ends_with('/') {
            tag_end + 1
        } else {
            let close = format!("</{name}>");
            match text[tag_end..].find(&close) {
                Some(offset) => tag_end + offset + close.len(),
                None => return false,
            }
        };
        text.replace_range(start..end, element);
        return true;
    }
    false
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            ch if (ch as u32) < 0x20 && !matches!(ch, '\t' | '\n' | '\r') => {}
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKET: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:pdf="http://ns.adobe.com/pdf/1.3/">
<dc:format>application/pdf</dc:format>
<dc:title><rdf:Alt><rdf:li xml:lang="x-default">Untitled</rdf:li></rdf:Alt></dc:title>
<pdf:Producer>Scanner 2.0</pdf:Producer>
</rdf:Description>
</rdf:RDF></x:xmpmeta>
<?xpacket end="w"?>"#;

    fn rewritten(title: Option<&str>, author: Option<&str>) -> String {
        String::from_utf8(rewrite(PACKET.as_bytes(), title, author).unwrap()).unwrap()
    }

    #[test]
    fn an_existing_title_is_replaced_in_place() {
        let out = rewritten(Some("Winter Harvest"), None);
        assert!(out.contains(
            "<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Winter Harvest</rdf:li></rdf:Alt></dc:title>"
        ));
        assert!(!out.contains("Untitled"));
        assert!(out.contains("<pdf:Producer>Scanner 2.0</pdf:Producer>"));
        assert!(!out.contains("dc:creator"));
    }

    #[test]
    fn a_missing_creator_is_added_in_a_new_description() {
        let out = rewritten(None, Some("Clara Mendes"));
        assert!(
            out.contains(
                "<dc:creator><rdf:Seq><rdf:li>Clara Mendes</rdf:li></rdf:Seq></dc:creator>"
            )
        );
        assert!(out.contains("Untitled"));
        let creator = out.find("dc:creator").unwrap();
        assert!(creator < out.find("</rdf:RDF>").unwrap());
    }

    #[test]
    fn values_are_escaped() {
        let out = rewritten(Some("Salt & <Pepper>"), Some("O'Neil"));
        assert!(out.contains("Salt &amp; &lt;Pepper&gt;"));
        assert!(out.contains("O&apos;Neil"));
    }

    #[test]
    fn a_self_closing_element_is_replaced() {
        let packet = PACKET.replace(
            "<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Untitled</rdf:li></rdf:Alt></dc:title>",
            "<dc:title/>",
        );
        let out = String::from_utf8(rewrite(packet.as_bytes(), Some("T"), None).unwrap()).unwrap();
        assert!(out.contains(">T</rdf:li>"));
        assert!(!out.contains("<dc:title/>"));
    }

    #[test]
    fn a_packet_without_rdf_is_left_alone() {
        assert!(rewrite(b"<x:xmpmeta/>", Some("T"), None).is_none());
        assert!(rewrite(b"\xff\xfe<rdf:RDF></rdf:RDF>", Some("T"), None).is_none());
    }
}
