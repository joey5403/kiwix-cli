use std::collections::HashMap;

use anyhow::{Context, Result};
use html2text::config::{self, ImageRenderMode};
use html2text::render::{RichAnnotation, TaggedLineElement};
use percent_encoding::percent_decode_str;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Link,
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentAction {
    pub kind: ActionKind,
    pub target: String,
    pub label: String,
    hits: Vec<ActionHit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActionHit {
    line: usize,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleAction {
    pub action: usize,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone)]
struct DocumentSpan {
    text: String,
    style: Style,
    action: Option<usize>,
}

#[derive(Debug, Clone, Default)]
struct DocumentLine {
    spans: Vec<DocumentSpan>,
}

#[derive(Debug, Clone, Default)]
pub struct ArticleDocument {
    lines: Vec<DocumentLine>,
    actions: Vec<DocumentAction>,
    fragments: HashMap<String, usize>,
}

impl ArticleDocument {
    pub fn from_html(html: &str, width: usize) -> Result<Self> {
        let rendered = config::rich()
            .empty_img_mode(ImageRenderMode::Filename)
            .lines_from_read(html.as_bytes(), width.max(20))
            .context("failed to render article HTML")?;
        let mut document = Self::default();
        let mut previous_action: Option<(ActionKind, String, usize)> = None;

        for (line_index, tagged_line) in rendered.into_iter().enumerate() {
            let mut line = DocumentLine::default();
            let mut column = 0;
            for element in tagged_line.iter() {
                match element {
                    TaggedLineElement::FragmentStart(fragment) => {
                        document
                            .fragments
                            .entry(fragment.clone())
                            .or_insert(line_index);
                    }
                    TaggedLineElement::Str(tagged) => {
                        let semantic = semantic_action(&tagged.tag);
                        let mut text = tagged.s.clone();
                        let action = semantic.map(|(kind, target)| {
                            let action_id = match &previous_action {
                                Some((previous_kind, previous_target, action_id))
                                    if *previous_kind == kind && previous_target == target =>
                                {
                                    *action_id
                                }
                                _ => {
                                    let action_id = document.actions.len();
                                    document.actions.push(DocumentAction {
                                        kind,
                                        target: target.to_owned(),
                                        label: text.trim().to_owned(),
                                        hits: Vec::new(),
                                    });
                                    action_id
                                }
                            };
                            if kind == ActionKind::Image
                                && document.actions[action_id].hits.is_empty()
                            {
                                text = format!("[image] {text}");
                                document.actions[action_id].label.clone_from(&text);
                            } else if document.actions[action_id].label.is_empty() {
                                text.trim()
                                    .clone_into(&mut document.actions[action_id].label);
                            }
                            previous_action = Some((kind, target.to_owned(), action_id));
                            action_id
                        });
                        if action.is_none() && !text.is_empty() {
                            previous_action = None;
                        }
                        let width = UnicodeWidthStr::width(text.as_str());
                        if let Some(action_id) = action
                            && width > 0
                        {
                            document.actions[action_id].hits.push(ActionHit {
                                line: line_index,
                                start: column,
                                end: column + width,
                            });
                        }
                        line.spans.push(DocumentSpan {
                            text,
                            style: annotation_style(&tagged.tag, action),
                            action,
                        });
                        column += width;
                    }
                }
            }
            apply_heading_style(&mut line);
            if line.spans.is_empty() {
                previous_action = None;
            }
            document.lines.push(line);
        }
        Ok(document)
    }

    #[must_use]
    pub fn lines(&self, selected_action: Option<usize>) -> Vec<Line<'static>> {
        self.lines
            .iter()
            .map(|line| {
                Line::from(
                    line.spans
                        .iter()
                        .map(|span| {
                            let style =
                                if selected_action.is_some() && span.action == selected_action {
                                    span.style.add_modifier(Modifier::BOLD | Modifier::REVERSED)
                                } else {
                                    span.style
                                };
                            Span::styled(span.text.clone(), style)
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    #[must_use]
    pub fn actions(&self) -> &[DocumentAction] {
        &self.actions
    }

    #[must_use]
    pub fn action_at(&self, line: usize, column: usize) -> Option<usize> {
        self.actions.iter().position(|action| {
            action
                .hits
                .iter()
                .any(|hit| hit.line == line && (hit.start..hit.end).contains(&column))
        })
    }

    #[must_use]
    pub fn action_line(&self, action: usize) -> Option<usize> {
        self.actions
            .get(action)
            .and_then(|action| action.hits.first())
            .map(|hit| hit.line)
    }

    #[must_use]
    pub fn visible_actions(&self, first_line: usize, line_count: usize) -> Vec<VisibleAction> {
        let end_line = first_line.saturating_add(line_count);
        self.actions
            .iter()
            .enumerate()
            .filter_map(|(action, item)| {
                item.hits
                    .iter()
                    .find(|hit| (first_line..end_line).contains(&hit.line))
                    .map(|hit| VisibleAction {
                        action,
                        line: hit.line,
                        column: hit.start,
                    })
            })
            .collect()
    }

    #[must_use]
    pub fn fragment_line(&self, fragment: &str) -> Option<usize> {
        self.fragments.get(fragment).copied().or_else(|| {
            let decoded = percent_decode_str(fragment).decode_utf8_lossy();
            self.fragments.get(decoded.as_ref()).copied()
        })
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
}

fn semantic_action(annotations: &[RichAnnotation]) -> Option<(ActionKind, &str)> {
    annotations
        .iter()
        .find_map(|annotation| match annotation {
            RichAnnotation::Image(target) => Some((ActionKind::Image, target.as_str())),
            _ => None,
        })
        .or_else(|| {
            annotations.iter().find_map(|annotation| match annotation {
                RichAnnotation::Link(target) => Some((ActionKind::Link, target.as_str())),
                _ => None,
            })
        })
}

fn annotation_style(annotations: &[RichAnnotation], action: Option<usize>) -> Style {
    let mut style = Style::default().fg(Color::Gray);
    for annotation in annotations {
        style = match annotation {
            RichAnnotation::Strong => style.add_modifier(Modifier::BOLD).fg(Color::White),
            RichAnnotation::Emphasis => style.add_modifier(Modifier::ITALIC).fg(Color::LightYellow),
            RichAnnotation::Strikeout => style.add_modifier(Modifier::CROSSED_OUT),
            RichAnnotation::Code | RichAnnotation::Preformat(_) => {
                style.fg(Color::LightGreen).add_modifier(Modifier::DIM)
            }
            RichAnnotation::Link(_) => style
                .fg(Color::LightBlue)
                .add_modifier(Modifier::UNDERLINED),
            RichAnnotation::Image(_) => style
                .fg(Color::LightMagenta)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            _ => style,
        };
    }
    if action.is_some() {
        style.add_modifier(Modifier::UNDERLINED)
    } else {
        style
    }
}

fn apply_heading_style(line: &mut DocumentLine) {
    let text = line
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect::<String>();
    let trimmed = text.trim_start();
    let level = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&level) || !trimmed[level..].starts_with(' ') {
        return;
    }
    let color = if level <= 2 {
        Color::LightCyan
    } else {
        Color::LightYellow
    };
    for span in &mut line.spans {
        span.style = span.style.fg(color).add_modifier(Modifier::BOLD);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_engine_preserves_styles_links_images_and_fragments() {
        let html = r#"<h1 id="top">Title</h1><p><strong>bold</strong> and
          <a href="Other#part">other</a> <img src="./_assets_/map.jpg" alt="Map"></p>"#;
        let document = ArticleDocument::from_html(html, 60).unwrap();

        assert_eq!(document.fragment_line("top"), Some(0));
        assert_eq!(document.actions().len(), 2);
        assert_eq!(document.actions()[0].kind, ActionKind::Link);
        assert_eq!(document.actions()[0].target, "Other#part");
        assert_eq!(document.actions()[1].kind, ActionKind::Image);
        assert_eq!(document.actions()[1].target, "./_assets_/map.jpg");
        assert!(document.lines(None).iter().any(|line| {
            line.spans.iter().any(|span| {
                span.content.contains("bold") && span.style.add_modifier.contains(Modifier::BOLD)
            })
        }));
    }

    #[test]
    fn action_hit_testing_uses_terminal_columns() {
        let document = ArticleDocument::from_html("<p>中<a href='Next'>link</a></p>", 60).unwrap();
        let action_line = document.action_line(0).unwrap();
        assert_eq!(document.action_at(action_line, 2), Some(0));
        assert_eq!(document.action_at(action_line, 6), None);
    }

    #[test]
    fn visible_actions_only_returns_targets_inside_the_viewport() {
        let document = ArticleDocument::from_html(
            "<p><a href='One'>one</a></p><p>gap</p><p><a href='Two'>two</a></p>",
            60,
        )
        .unwrap();
        let second_line = document.action_line(1).unwrap();

        let visible = document.visible_actions(second_line, 1);

        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].action, 1);
        assert_eq!(visible[0].line, second_line);
    }

    #[test]
    fn article_styles_never_force_a_background_colour() {
        let document =
            ArticleDocument::from_html("<p>body <code>code</code> <a href='Next'>link</a></p>", 60)
                .unwrap();

        for line in document.lines(Some(0)) {
            assert!(line.spans.iter().all(|span| span.style.bg.is_none()));
        }
    }

    #[test]
    fn no_action_selected_does_not_highlight_plain_body_spans() {
        let document =
            ArticleDocument::from_html("<p>plain body <a href='Next'>interactive link</a></p>", 60)
                .unwrap();

        assert!(document.lines(None).iter().all(|line| {
            line.spans
                .iter()
                .all(|span| !span.style.add_modifier.contains(Modifier::REVERSED))
        }));
        assert!(document.lines(Some(0)).iter().any(|line| {
            line.spans.iter().any(|span| {
                span.content.contains("interactive link")
                    && span.style.add_modifier.contains(Modifier::REVERSED)
            })
        }));
    }
}
