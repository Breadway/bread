//! A small syntax-highlighted, read-only JSON view.
//!
//! Deliberately not GtkSourceView: breadscope should not add another system
//! library dependency for one pane. The tokenizer is enough for pretty-printed
//! JSON from the daemon.

use gtk4::prelude::*;
use gtk4::{TextBuffer, TextView, WrapMode};

const KEY: &str = "json-key";
const STRING: &str = "json-string";
const NUMBER: &str = "json-number";
const BOOL: &str = "json-bool";
const NULL: &str = "json-null";

pub struct JsonView {
    pub root: gtk4::ScrolledWindow,
    buffer: TextBuffer,
}

impl JsonView {
    pub fn new(height: i32) -> Self {
        let view = TextView::new();
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_monospace(true);
        view.set_wrap_mode(WrapMode::None);
        view.set_top_margin(8);
        view.set_bottom_margin(8);
        view.set_left_margin(10);
        view.set_right_margin(10);
        view.add_css_class("json-view");

        let buffer = view.buffer();
        let table = buffer.tag_table();
        for (name, color) in [
            (KEY, "#9ecbff"),
            (STRING, "#c3e88d"),
            (NUMBER, "#f0b37e"),
            (BOOL, "#c792ea"),
            (NULL, "#7a7a7a"),
        ] {
            let tag = gtk4::TextTag::new(Some(name));
            tag.set_foreground(Some(color));
            if name == KEY {
                tag.set_weight(600);
            }
            table.add(&tag);
        }

        let scroll = gtk4::ScrolledWindow::new();
        scroll.set_child(Some(&view));
        scroll.set_min_content_height(height);
        scroll.set_max_content_height(height);
        scroll.set_policy(gtk4::PolicyType::Automatic, gtk4::PolicyType::Automatic);
        scroll.set_hexpand(true);
        scroll.set_vexpand(true);

        Self {
            root: scroll,
            buffer,
        }
    }

    pub fn set_value(&self, value: &serde_json::Value) {
        let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
        self.set_text(&text);
    }

    pub fn set_text(&self, text: &str) {
        self.buffer.set_text("");
        highlight(&self.buffer, text);
    }
}

fn highlight(buffer: &TextBuffer, text: &str) {
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0usize;
    let mut plain = String::new();

    let flush = |buffer: &TextBuffer, plain: &mut String| {
        if plain.is_empty() {
            return;
        }
        let mut end = buffer.end_iter();
        buffer.insert(&mut end, plain);
        plain.clear();
    };

    while index < chars.len() {
        let character = chars[index];

        if character == '"' {
            flush(buffer, &mut plain);
            let start = index;
            index += 1;
            while index < chars.len() {
                if chars[index] == '\\' {
                    index = (index + 2).min(chars.len());
                    continue;
                }
                if chars[index] == '"' {
                    index += 1;
                    break;
                }
                index += 1;
            }
            let token: String = chars[start..index].iter().collect();
            let mut lookahead = index;
            while lookahead < chars.len() && chars[lookahead].is_whitespace() {
                lookahead += 1;
            }
            let tag = if lookahead < chars.len() && chars[lookahead] == ':' {
                KEY
            } else {
                STRING
            };
            let mut end = buffer.end_iter();
            buffer.insert_with_tags_by_name(&mut end, &token, &[tag]);
            continue;
        }

        if character.is_ascii_digit() || (character == '-' && index + 1 < chars.len()) {
            flush(buffer, &mut plain);
            let start = index;
            index += 1;
            while index < chars.len()
                && (chars[index].is_ascii_digit()
                    || matches!(chars[index], '.' | 'e' | 'E' | '+' | '-'))
            {
                index += 1;
            }
            let token: String = chars[start..index].iter().collect();
            let mut end = buffer.end_iter();
            buffer.insert_with_tags_by_name(&mut end, &token, &[NUMBER]);
            continue;
        }

        if character.is_ascii_alphabetic() {
            let start = index;
            while index < chars.len() && chars[index].is_ascii_alphabetic() {
                index += 1;
            }
            let token: String = chars[start..index].iter().collect();
            if matches!(token.as_str(), "true" | "false") {
                flush(buffer, &mut plain);
                let mut end = buffer.end_iter();
                buffer.insert_with_tags_by_name(&mut end, &token, &[BOOL]);
            } else if token == "null" {
                flush(buffer, &mut plain);
                let mut end = buffer.end_iter();
                buffer.insert_with_tags_by_name(&mut end, &token, &[NULL]);
            } else {
                plain.push_str(&token);
            }
            continue;
        }

        plain.push(character);
        index += 1;
    }
    flush(buffer, &mut plain);
}

#[cfg(test)]
mod tests {
    // The tokenizer lives in GTK objects, so there is no headless unit test
    // here. The state/modules views exercise it on every refresh.
}
