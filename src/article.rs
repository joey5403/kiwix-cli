use std::collections::HashMap;

use anyhow::{Context, Result};
use html2text::config::{self, ImageRenderMode};
use html2text::render::{RichAnnotation, TaggedLineElement};
use percent_encoding::percent_decode_str;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordPosition {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub text: String,
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
    #[cfg(test)]
    pub fn lines(&self, selected_action: Option<usize>) -> Vec<Line<'static>> {
        self.lines_with_word(selected_action, None)
    }

    #[must_use]
    pub fn lines_with_word(
        &self,
        selected_action: Option<usize>,
        selected_word: Option<&WordPosition>,
    ) -> Vec<Line<'static>> {
        self.lines
            .iter()
            .enumerate()
            .map(|(line_index, line)| {
                let mut column: usize = 0;
                Line::from(
                    line.spans
                        .iter()
                        .flat_map(|span| {
                            let width = UnicodeWidthStr::width(span.text.as_str());
                            let selected_action =
                                selected_action.is_some() && span.action == selected_action;
                            let word = selected_word.filter(|word| word.line == line_index);
                            let Some(word) = word else {
                                column = column.saturating_add(width);
                                return vec![Span::styled(
                                    span.text.clone(),
                                    if selected_action {
                                        span.style.add_modifier(Modifier::BOLD | Modifier::REVERSED)
                                    } else {
                                        span.style
                                    },
                                )];
                            };
                            let mut pieces = Vec::new();
                            let mut segment = String::new();
                            let mut segment_selected = None;
                            for character in span.text.chars() {
                                let character_width =
                                    UnicodeWidthChar::width(character).unwrap_or(0);
                                let word_selected = if character_width == 0 {
                                    word.start <= column && column <= word.end
                                } else {
                                    column < word.end
                                        && word.start < column.saturating_add(character_width)
                                };
                                let selected = selected_action || word_selected;
                                if segment_selected.is_some_and(|current| current != selected) {
                                    let style = if segment_selected == Some(true) {
                                        span.style.add_modifier(Modifier::BOLD | Modifier::REVERSED)
                                    } else {
                                        span.style
                                    };
                                    pieces.push(Span::styled(std::mem::take(&mut segment), style));
                                }
                                segment_selected = Some(selected);
                                segment.push(character);
                                column = column.saturating_add(character_width);
                            }
                            if !segment.is_empty() {
                                let style = if segment_selected == Some(true) {
                                    span.style.add_modifier(Modifier::BOLD | Modifier::REVERSED)
                                } else {
                                    span.style
                                };
                                pieces.push(Span::styled(segment, style));
                            }
                            pieces
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
    pub fn word_positions(&self, line: usize) -> Vec<WordPosition> {
        let Some(document_line) = self.lines.get(line) else {
            return Vec::new();
        };
        let mut characters = Vec::new();
        let mut column: usize = 0;
        for span in &document_line.spans {
            for character in span.text.chars() {
                let width = UnicodeWidthChar::width(character).unwrap_or(0);
                characters.push((character, column, column.saturating_add(width)));
                column = column.saturating_add(width);
            }
        }

        let mut words = Vec::new();
        let mut index = 0;
        while index < characters.len() {
            let character = characters[index].0;
            if is_han(character) {
                let (_, start, _) = characters[index];
                let end = characters[index].2;
                words.push(WordPosition {
                    line,
                    start,
                    end,
                    text: character.to_string(),
                });
                index += 1;
                continue;
            }
            if !is_word_character(&characters, index) {
                index += 1;
                continue;
            }

            let start_index = index;
            index += 1;
            while index < characters.len()
                && !is_han(characters[index].0)
                && is_word_character(&characters, index)
            {
                index += 1;
            }
            let text = characters[start_index..index]
                .iter()
                .map(|(character, _, _)| character)
                .collect();
            words.push(WordPosition {
                line,
                start: characters[start_index].1,
                end: characters[index - 1].2,
                text,
            });
        }
        words
    }

    #[must_use]
    pub fn word_at(&self, line: usize, column: usize) -> Option<WordPosition> {
        self.word_positions(line)
            .into_iter()
            .find(|word| (word.start..word.end).contains(&column))
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

fn is_word_character(characters: &[(char, usize, usize)], index: usize) -> bool {
    let character = characters[index].0;
    character.is_alphanumeric()
        || is_combining_mark(character)
        || character == '_'
        || (matches!(character, '\'' | '’')
            && index > 0
            && index + 1 < characters.len()
            && characters[index - 1].0.is_alphanumeric()
            && characters[index + 1].0.is_alphanumeric())
}

fn is_combining_mark(character: char) -> bool {
    matches!(
        u32::from(character),
        0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0xfe20..=0xfe2f
    )
}

fn is_han(character: char) -> bool {
    matches!(
        u32::from(character),
        0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xf900..=0xfaff
            | 0x20000..=0x2fa1f
            | 0x30000..=0x323af
    )
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
    fn word_hit_testing_uses_display_columns_and_unicode_words() {
        let document =
            ArticleDocument::from_html("<p>Hi 中，don't stop! cafe\u{301}</p>", 60).unwrap();
        let words = document.word_positions(0);
        let text = words
            .iter()
            .map(|word| word.text.as_str())
            .collect::<Vec<_>>();

        assert!(text.contains(&"Hi"));
        assert!(text.contains(&"中"));
        assert!(text.contains(&"don't"));
        assert!(text.contains(&"stop"));
        assert!(text.contains(&"cafe\u{301}"));
        let chinese = words.iter().find(|word| word.text == "中").unwrap();
        assert_eq!(document.word_at(0, chinese.start), Some(chinese.clone()));
    }

    #[test]
    fn selected_word_highlights_only_the_word_inside_a_sentence_span() {
        let document = ArticleDocument::from_html("<p>one two three</p>", 60).unwrap();
        let selected = document.word_at(0, 5).unwrap();
        let highlighted = document.lines_with_word(None, Some(&selected));
        let selected_text = highlighted[0]
            .spans
            .iter()
            .filter(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(selected.text, "two");
        assert_eq!(selected_text, "two");
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
