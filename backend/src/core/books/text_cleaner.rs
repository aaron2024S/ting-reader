//! Text cleaning and normalization module
//!
//! This module provides built-in text cleaning functionality for chapter titles
//! and filenames. It includes:
//! - Regex-based cleaning rules for special characters
//! - Chapter number normalization (Chinese and English formats)
//! - Advertisement text removal
//! - Path safety checks (prevent path traversal)
//! - Filename length limits and special character filtering
//!
//! This is a core system feature, not implemented as a plugin.

use crate::core::app::error::{Result, TingError};
use regex::Regex;
use std::cmp::Ordering;
use std::sync::OnceLock;

/// Extract the title from a directory/search name using "Title-Contributor"
/// or "Title | Description". Explicit metadata titles do not use this fallback.
pub(crate) fn book_title_from_name(name: &str) -> &str {
    BookName::parse(name).title
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct BookName<'a> {
    pub title: &'a str,
    pub author: Option<&'a str>,
    pub narrator: Option<&'a str>,
}

impl<'a> BookName<'a> {
    /// Title-Author[/Narrator]. A single unlabeled name defaults to author.
    /// Fullwidth slash and a second hyphen work in actual directory names.
    pub fn parse(name: &'a str) -> Self {
        let name = name.trim();
        let name = name
            .split(['|', '丨', '｜'])
            .next()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or(name);
        let mut parsed = Self {
            title: name,
            author: None,
            narrator: None,
        };
        for (index, _) in name.match_indices(['-', '－', '—']) {
            let title = name[..index].trim();
            let contributors = name[index..].trim_start_matches(['-', '－', '—']).trim();
            let token = contributors
                .split(['/', '／', '-', '－', '—'])
                .next()
                .unwrap_or_default();
            if is_volume_marker(token) {
                continue;
            }
            // A numeric volume suffix belongs to the title.
            if !title.is_empty()
                && !contributors.is_empty()
                && !contributors.chars().all(|ch| ch.is_numeric())
            {
                parsed.title = title;
                let (author, narrator) = contributors
                    .split_once(['/', '／', '-', '－', '—'])
                    .map_or((contributors, ""), |parts| parts);
                for (value, is_narrator) in [(author, false), (narrator, true)] {
                    let value = value.trim();
                    let (label, value) = value
                        .split_once([':', '：'])
                        .map_or(("", value), |(label, value)| (label.trim(), value.trim()));
                    if value.is_empty() {
                        continue;
                    }
                    match label {
                        "作者" | "原著" | "author" => parsed.author = Some(value),
                        "演播" | "主播" | "播讲" | "narrator" => {
                            parsed.narrator = Some(value)
                        }
                        _ if is_narrator => parsed.narrator = Some(value),
                        _ => parsed.author = Some(value),
                    }
                }
                break;
            }
        }
        parsed
    }
}

fn is_volume_marker(value: &str) -> bool {
    static VOLUME: OnceLock<Regex> = OnceLock::new();
    VOLUME.get_or_init(|| Regex::new(
        r"(?ix)^\s*(?:[0-9０-９]+|s[0-9０-９]{1,3}|(?:vol(?:ume)?|season|book)\s*\.?\s*[0-9０-９]{1,3}|(?:第\s*)?[0-9０-９零〇一二两三四五六七八九十百千万]+\s*(?:卷|季|部|册|輯|辑))\s*$",
    ).unwrap()).is_match(value)
}

/// Shared positive/zero ordinal parser for series and chapter filenames.
/// Checked arithmetic rejects malformed/overflowing input instead of panicking.
pub(crate) fn parse_ordinal(value: &str) -> Option<i32> {
    let value: String = value
        .trim()
        .chars()
        .map(|ch| match ch {
            '０'..='９' => char::from(b'0' + (ch as u32 - '０' as u32) as u8),
            _ => ch,
        })
        .collect();
    if value.is_empty() {
        return None;
    }
    let digit = |ch| match ch {
        '0'..='9' => Some(ch as i32 - '0' as i32),
        '零' | '〇' => Some(0),
        '一' => Some(1),
        '二' | '两' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        _ => None,
    };
    if value.chars().all(|ch| digit(ch).is_some()) {
        return value.chars().try_fold(0i32, |number, ch| {
            number.checked_mul(10)?.checked_add(digit(ch)?)
        });
    }
    let mut total = 0i32;
    let mut section = 0i32;
    let mut number = 0i32;
    let mut previous_unit = 10000;
    let mut has_ten_thousands = false;
    for ch in value.chars() {
        if let Some(next) = digit(ch) {
            number = number.checked_mul(10)?.checked_add(next)?;
            continue;
        }
        let unit = match ch {
            '十' => 10,
            '百' => 100,
            '千' => 1000,
            '万' => 10000,
            _ => return None,
        };
        if unit == 10000 {
            if has_ten_thousands {
                return None;
            }
            section = section.checked_add(number)?;
            total = total.checked_add(section.max(1).checked_mul(unit)?)?;
            section = 0;
            previous_unit = 10000;
            has_ten_thousands = true;
        } else {
            if unit >= previous_unit {
                return None;
            }
            section = section.checked_add(number.max(1).checked_mul(unit)?)?;
            previous_unit = unit;
        }
        number = 0;
    }
    total.checked_add(section)?.checked_add(number)
}

fn chapter_number_match(title: &str) -> Option<(i32, usize)> {
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    let regex = NUMBER.get_or_init(|| Regex::new(
        r"(?ix)(?:第\s*([0-9０-９零〇一二两三四五六七八九十百千万]+)\s*[章节集回话話節讲講]|([0-9０-９零〇一二两三四五六七八九十百千万]+)\s*[章节集回话話節讲講]|\b(?:chapter|episode|ep)\s*([0-9０-９]+))|^\s*([0-9０-９零〇一二两三四五六七八九十百千万]+)(?:[\s._、:：\-]|$)",
    ).unwrap());
    let caps = regex.captures(title)?;
    let number = caps.iter().skip(1).flatten().next()?;
    Some((parse_ordinal(number.as_str())?, caps.get(0)?.end()))
}

/// Cached key shared by scans, chapter merges and sidecar offsets.
/// Compare decoded paths naturally; numbered filenames use their numeric
/// chapter position, including Chinese ordinals. Directory segments retain
/// their order so chapter counters can restart inside separate ranges.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ChapterOrderKey {
    normalized: String,
    original: String,
}

impl Ord for ChapterOrderKey {
    fn cmp(&self, other: &Self) -> Ordering {
        natord::compare(&self.normalized, &other.normalized)
            .then_with(|| self.original.cmp(&other.original))
    }
}

impl PartialOrd for ChapterOrderKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub(crate) fn chapter_order_key(path: &str) -> ChapterOrderKey {
    let url = (path.starts_with("http://") || path.starts_with("https://"))
        .then(|| url::Url::parse(path).ok())
        .flatten();
    let decoded = url
        .as_ref()
        .map(|url| urlencoding::decode(url.path()).unwrap_or_else(|_| url.path().into()));
    let normalized = decoded.as_deref().unwrap_or(path).replace('\\', "/");
    let (parent, filename) = normalized.rsplit_once('/').unwrap_or(("", &normalized));
    let normalized = if let Some((number, end)) = chapter_number_match(filename) {
        format!("{parent}/{number} {}", &filename[end..])
    } else {
        normalized
    };
    ChapterOrderKey {
        normalized,
        original: path.to_owned(),
    }
}

/// Text cleaner for chapter titles and filenames
pub struct TextCleaner {
    builtin_rules: Vec<CleaningRule>,
    plugin_rules: Vec<CleaningRule>,
    config: CleanerConfig,
}

/// A single cleaning rule with regex pattern and replacement
#[derive(Debug, Clone)]
pub struct CleaningRule {
    pub name: String,
    pub priority: u32,
    pub pattern: Regex,
    pub replacement: String,
}

/// Result of applying cleaning rules
#[derive(Debug, Clone)]
pub struct CleaningResult {
    pub original: String,
    pub cleaned: String,
    pub applied_rules: Vec<String>,
}

/// Configuration for the text cleaner
#[derive(Debug, Clone)]
pub struct CleanerConfig {
    pub max_filename_length: usize,
    pub allowed_chars: String,
    pub custom_rules: Vec<CleaningRule>,
}

impl Default for CleanerConfig {
    fn default() -> Self {
        Self {
            max_filename_length: 255,
            allowed_chars: String::from(
                "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-. ",
            ),
            custom_rules: Vec::new(),
        }
    }
}

impl TextCleaner {
    pub(crate) fn clean_book_directory_title(&self, name: &str) -> String {
        self.clean_chapter_title(book_title_from_name(name), None).0
    }

    /// Create a new text cleaner with the given configuration
    pub fn new(config: CleanerConfig) -> Self {
        let builtin_rules = Self::create_builtin_rules();

        Self {
            builtin_rules,
            plugin_rules: Vec::new(),
            config,
        }
    }

    /// Create built-in cleaning rules
    fn create_builtin_rules() -> Vec<CleaningRule> {
        vec![
            // Rule 1: Remove special characters that are invalid in filenames
            CleaningRule {
                name: "remove_special_chars".to_string(),
                priority: 100,
                pattern: Regex::new(r#"[<>:"/\\|?*]"#).unwrap(),
                replacement: "_".to_string(),
            },
            // Rule 2a: Normalize Chinese episode numbers to Episode format
            // CleaningRule {
            //     name: "normalize_chinese_episode".to_string(),
            //     priority: 90,
            //     pattern: Regex::new(r"第(\d+)集").unwrap(),
            //     replacement: "Episode $1".to_string(),
            // },
            // Rule 2b: Remove pipe-enclosed text (e.g. |Title|)
            CleaningRule {
                name: "remove_pipe_enclosed".to_string(),
                priority: 101, // Must run before special chars removal
                pattern: Regex::new(r"\|.*?\|").unwrap(),
                replacement: "".to_string(),
            },
            // Rule 2: Normalize Chinese chapter numbers to English format
            // CleaningRule {
            //     name: "normalize_chinese_chapter".to_string(),
            //     priority: 90,
            //     pattern: Regex::new(r"第(\d+)章").unwrap(),
            //     replacement: "Chapter $1".to_string(),
            // },
            // Rule 3: Normalize English chapter numbers (case-insensitive)
            // CleaningRule {
            //     name: "normalize_english_chapter".to_string(),
            //     priority: 89,
            //     pattern: Regex::new(r"(?i)chapter\s+(\d+)").unwrap(),
            //     replacement: "Chapter $1".to_string(),
            // },
            // Rule 4: Remove leading zeros from chapter numbers
            CleaningRule {
                name: "remove_leading_zeros".to_string(),
                priority: 88,
                pattern: Regex::new(r"^0+(\d+)").unwrap(),
                replacement: "$1".to_string(),
            },
            // Rule 5: Remove "喜马拉雅" advertisement
            CleaningRule {
                name: "remove_ximalaya_ad".to_string(),
                priority: 80,
                pattern: Regex::new(r"喜马拉雅").unwrap(),
                replacement: "".to_string(),
            },
            // Rule 6: Remove "VIP专享" advertisement
            CleaningRule {
                name: "remove_vip_ad".to_string(),
                priority: 79,
                pattern: Regex::new(r"VIP专享").unwrap(),
                replacement: "".to_string(),
            },
            // Rule 7: Remove "付费内容" advertisement
            CleaningRule {
                name: "remove_paid_content_ad".to_string(),
                priority: 78,
                pattern: Regex::new(r"付费内容").unwrap(),
                replacement: "".to_string(),
            },
            // Rule 8: Remove bracketed advertisement text
            CleaningRule {
                name: "remove_bracketed_ads".to_string(),
                priority: 77,
                pattern: Regex::new(r"\[.*?广告.*?\]").unwrap(),
                replacement: "".to_string(),
            },
            // Rule 9: Remove "Extra" markers (番外, etc.)
            // CleaningRule {
            //     name: "remove_extra_markers".to_string(),
            //     priority: 76,
            //     pattern: Regex::new(r"(?i)(番外|花絮|特典|SP|Extra)[：:\-\s]*").unwrap(),
            //     replacement: "".to_string(),
            // },
            // Rule 10: Remove common promotional suffixes and advertisements
            CleaningRule {
                name: "remove_promo_keywords".to_string(),
                priority: 75,
                // '请?订阅', '求订阅', '转发', '五星', '好评', '关注', '微信', '群', '更多', '加我', '联系', '点击', '搜新书', '新书', '推荐', '上架', '完本'
                pattern: Regex::new(r"[（\(\[\{【](?:(?:请|求)?订阅|转发|五星|好评|关注|微信|群|更多|加我|联系|点击|搜新书|新书|推荐|上架|完本).*?[）\)\]\}】]").unwrap(),
                replacement: "".to_string(),
            },
            // Rule 11: Remove common suffixes: "-ZmAudio"
            CleaningRule {
                name: "remove_zmaudio_suffix".to_string(),
                priority: 74,
                pattern: Regex::new(r"(?i)[-_]ZmAudio$").unwrap(),
                replacement: "".to_string(),
            },
            // Rule 12: Trim whitespace
            CleaningRule {
                name: "trim_whitespace".to_string(),
                priority: 10,
                pattern: Regex::new(r"^\s+|\s+$").unwrap(),
                replacement: "".to_string(),
            },
            // Rule 13: Collapse multiple spaces
            CleaningRule {
                name: "collapse_spaces".to_string(),
                priority: 9,
                pattern: Regex::new(r"\s+").unwrap(),
                replacement: " ".to_string(),
            },
        ]
    }

    /// Extract chapter number from title
    pub fn extract_chapter_number(&self, title: &str) -> Option<i32> {
        chapter_number_match(title).map(|(number, _)| number)
    }

    /// Clean a chapter title
    /// Returns: (cleaned_title, is_extra)
    pub fn clean_chapter_title(&self, title: &str, _book_title: Option<&str>) -> (String, bool) {
        // Remove extension if present
        let title_no_ext = if let Some(idx) = title.rfind('.') {
            // Check if the part after dot looks like an extension (alphanumeric, length < 5)
            let ext = &title[idx + 1..];
            if !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                &title[..idx]
            } else {
                title
            }
        } else {
            title
        };

        // Detect "Extra" markers
        // Patterns: 番外, 花絮, 特典, SP, Extra
        let is_extra = Regex::new(r"(?i)番外|花絮|特典|SP|Extra")
            .unwrap()
            .is_match(title_no_ext);

        // Apply remaining builtin rules (like special chars removal)
        // But skip the ones we already handled or that might conflict
        let cleaned = self.apply_all_rules(title_no_ext).cleaned;

        (cleaned, is_extra)
    }

    /// Clean a filename
    pub fn clean_filename(&self, filename: &str) -> String {
        let mut cleaned = self.apply_all_rules(filename).cleaned;

        // Apply filename length limit
        if cleaned.len() > self.config.max_filename_length {
            cleaned.truncate(self.config.max_filename_length);
        }

        cleaned
    }

    /// Normalize chapter number format
    pub fn normalize_chapter_number(&self, text: &str) -> String {
        // Apply chapter normalization rules
        let mut result = text.to_string();

        for rule in &self.builtin_rules {
            if rule.name.contains("chapter") || rule.name.contains("leading_zeros") {
                result = rule
                    .pattern
                    .replace_all(&result, rule.replacement.as_str())
                    .to_string();
            }
        }

        result
    }

    /// Remove advertisement and irrelevant text
    pub fn remove_ads(&self, text: &str) -> String {
        let mut result = text.to_string();

        for rule in &self.builtin_rules {
            if rule.name.contains("ad") {
                result = rule
                    .pattern
                    .replace_all(&result, rule.replacement.as_str())
                    .to_string();
            }
        }

        result
    }

    /// Validate path for safety (prevent path traversal)
    pub fn validate_path(&self, path: &str) -> Result<()> {
        // Check for path traversal attempts
        if path.contains("..") {
            return Err(TingError::SecurityViolation(
                "Path traversal detected: '..' is not allowed".to_string(),
            ));
        }

        // Check for absolute paths (Unix)
        if path.starts_with('/') {
            return Err(TingError::SecurityViolation(
                "Absolute paths are not allowed".to_string(),
            ));
        }

        // Check for absolute paths (Windows)
        if path.len() >= 2 && path.chars().nth(1) == Some(':') {
            let first_char = path.chars().next().unwrap();
            if first_char.is_ascii_alphabetic() {
                return Err(TingError::SecurityViolation(
                    "Absolute paths are not allowed".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Register a plugin cleaning rule
    pub fn register_plugin_rule(&mut self, rule: CleaningRule) {
        self.plugin_rules.push(rule);
        // Sort by priority (higher priority first)
        self.plugin_rules
            .sort_by_key(|a| std::cmp::Reverse(a.priority));
    }

    /// Apply all cleaning rules (builtin first, then plugin rules)
    pub fn apply_all_rules(&self, text: &str) -> CleaningResult {
        let mut result = text.to_string();
        let mut applied_rules = Vec::new();

        // Combine builtin, plugin and custom rules, sorted by priority
        let mut all_rules: Vec<&CleaningRule> = self.builtin_rules.iter().collect();
        all_rules.extend(self.plugin_rules.iter());
        all_rules.extend(self.config.custom_rules.iter());
        all_rules.sort_by_key(|a| std::cmp::Reverse(a.priority));

        // Apply each rule
        for rule in all_rules {
            let before = result.clone();
            result = rule
                .pattern
                .replace_all(&result, rule.replacement.as_str())
                .to_string();

            // Track which rules were applied
            if before != result {
                applied_rules.push(rule.name.clone());
            }
        }

        CleaningResult {
            original: text.to_string(),
            cleaned: result,
            applied_rules,
        }
    }
}
