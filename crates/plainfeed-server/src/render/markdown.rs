use pulldown_cmark::{Event, Options, Parser, Tag, html};

use super::TrustedHtml;

pub(super) fn render(markdown: &str) -> TrustedHtml {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_GFM);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);

    let events = Parser::new_ext(markdown, options).filter_map(|event| match event {
        Event::Html(_) | Event::InlineHtml(_) => None,
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Link {
            link_type,
            dest_url: safe_destination(&dest_url).into(),
            title,
            id,
        })),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Image {
            link_type,
            dest_url: safe_destination(&dest_url).into(),
            title,
            id,
        })),
        event => Some(event),
    });
    let mut output = String::with_capacity(markdown.len().saturating_mul(3) / 2);
    html::push_html(&mut output, events);
    TrustedHtml(output)
}

fn safe_destination(destination: &str) -> String {
    let trimmed = destination.trim();
    let lowercase = trimmed.to_ascii_lowercase();
    if lowercase.starts_with("https://")
        || lowercase.starts_with("http://")
        || lowercase.starts_with("mailto:")
        || trimmed.starts_with('#')
    {
        trimmed.to_owned()
    } else {
        "#".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_reader_markdown_extensions() {
        let html = render("## Heading\n\n- [x] Done\n\n| A | B |\n| - | - |\n| 1 | 2 |");

        assert!(html.as_str().contains("<h2>Heading</h2>"));
        assert!(html.as_str().contains("type=\"checkbox\""));
        assert!(html.as_str().contains("<table>"));
    }

    #[test]
    fn removes_raw_html_and_blocks_unsafe_destinations() {
        let html = render(
            "<script>alert('raw')</script>\n\n[unsafe](javascript:alert(1)) [safe](https://example.com)",
        );

        assert!(!html.as_str().contains("<script"));
        assert!(!html.as_str().contains("javascript:"));
        assert!(html.as_str().contains("href=\"#\""));
        assert!(html.as_str().contains("href=\"https://example.com\""));
    }
}
