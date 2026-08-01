use std::{
    collections::HashSet,
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{self, Command},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde::Deserialize;

const EXIT_COMMAND_NOT_FOUND: i32 = 127;
const EXIT_INTERRUPTED: i32 = 130;
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_MENU_ITEMS: usize = 1_000;
const MAX_RECENT_ITEMS: usize = 20;
const SYSTEM_CONFIG_PATH: &str = "/etc/lazymenu-cli/menu.toml";

#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    #[serde(default)]
    menu: MenuSection,
    #[serde(default)]
    items: Vec<FileItem>,
}

#[derive(Debug, Deserialize, Default)]
struct MenuSection {
    title: Option<String>,
    quit_key: Option<String>,
    #[serde(rename = "loop")]
    loop_menu: Option<bool>,
    key_format: Option<String>,
    selected_foreground: Option<String>,
    selected_background: Option<String>,
    selected_bold: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct FileItem {
    #[serde(default)]
    id: Option<String>,
    label: String,
    command: CommandSpec,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    confirm: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    favorite: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CommandSpec {
    Shell(String),
    Direct(Vec<String>),
}

#[derive(Debug)]
struct MenuConfig {
    title: String,
    quit_key: String,
    loop_menu: bool,
    key_format: String,
    selected_foreground: Color,
    selected_background: Color,
    selected_bold: bool,
    items: Vec<MenuItem>,
}

#[derive(Debug)]
struct MenuItem {
    stable_id: String,
    label: String,
    command: CommandSpec,
    key: Option<String>,
    confirm: bool,
    description: Option<String>,
    group: Option<String>,
    tags: Vec<String>,
    favorite: bool,
}

struct RecentStore {
    path: Option<PathBuf>,
    ids: Vec<String>,
}

struct Options {
    config_path: Option<PathBuf>,
    dry_run: bool,
    print_items: bool,
}

struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    fn new() -> Self {
        Self { active: false }
    }

    fn enter(&mut self) -> io::Result<()> {
        terminal::enable_raw_mode()?;
        self.active = true;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, Hide) {
            let _ = self.leave();
            return Err(error);
        }
        Ok(())
    }

    fn leave(&mut self) -> io::Result<()> {
        if self.active {
            terminal::disable_raw_mode()?;
            execute!(
                io::stdout(),
                Show,
                ResetColor,
                SetAttribute(Attribute::Reset),
                LeaveAlternateScreen
            )?;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

fn config_error(message: impl Into<String>) -> String {
    format!("lazymenu-cli: {}", message.into())
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        config_path: None,
        dry_run: false,
        print_items: false,
    };
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--config" => {
                options.config_path = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| config_error("--config needs a path"))?,
                ));
            }
            "--dry-run" => options.dry_run = true,
            "--print" => options.print_items = true,
            "-h" | "--help" => {
                println!(
                    "Usage: lazymenu-cli [options]\n\nOptions:\n  --config PATH  TOML config file (default: ./menu.toml, then /etc/lazymenu-cli/menu.toml)\n  --dry-run      Show commands instead of executing them\n  --print        List configured items and exit\n  -h, --help     Show this help"
                );
                process::exit(0);
            }
            _ => return Err(config_error(format!("unknown option: {argument}"))),
        }
    }
    Ok(options)
}

fn one_character(value: &str) -> bool {
    value.chars().count() == 1
}

fn normalize_key(value: &str) -> String {
    value.to_lowercase()
}

fn stable_hash(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("auto:{hash:016x}")
}

fn valid_explicit_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
}

fn parse_color(value: &str, field: &str) -> Result<Color, String> {
    let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
    let color = match normalized.as_str() {
        "default" | "reset" => Color::Reset,
        "black" => Color::Black,
        "dark_grey" | "dark_gray" => Color::DarkGrey,
        "grey" | "gray" => Color::Grey,
        "white" => Color::White,
        "dark_red" => Color::DarkRed,
        "red" => Color::Red,
        "dark_green" => Color::DarkGreen,
        "green" => Color::Green,
        "dark_yellow" => Color::DarkYellow,
        "yellow" => Color::Yellow,
        "dark_blue" => Color::DarkBlue,
        "blue" => Color::Blue,
        "dark_magenta" => Color::DarkMagenta,
        "magenta" => Color::Magenta,
        "dark_cyan" => Color::DarkCyan,
        "cyan" => Color::Cyan,
        _ if normalized.starts_with('#') && normalized.len() == 7 => {
            let red = u8::from_str_radix(&normalized[1..3], 16).ok();
            let green = u8::from_str_radix(&normalized[3..5], 16).ok();
            let blue = u8::from_str_radix(&normalized[5..7], 16).ok();
            match (red, green, blue) {
                (Some(r), Some(g), Some(b)) => Color::Rgb { r, g, b },
                _ => {
                    return Err(config_error(format!(
                        "menu.{field} must be a named color or #RRGGBB"
                    )));
                }
            }
        }
        _ => {
            return Err(config_error(format!(
                "menu.{field} must be a named color or #RRGGBB"
            )));
        }
    };
    Ok(color)
}

fn format_binding(template: &str, key: &str) -> String {
    template.replace("{key}", key)
}

fn parse_config_text(text: &str) -> Result<MenuConfig, String> {
    let file_config: FileConfig =
        toml::from_str(text).map_err(|error| config_error(format!("invalid TOML: {error}")))?;
    let title = file_config.menu.title.unwrap_or_else(|| "Menu".to_owned());
    let quit_key = file_config.menu.quit_key.unwrap_or_else(|| "q".to_owned());
    let loop_menu = file_config.menu.loop_menu.unwrap_or(true);
    let key_format = file_config
        .menu
        .key_format
        .unwrap_or_else(|| "[{key}]".to_owned());
    let selected_foreground = parse_color(
        file_config
            .menu
            .selected_foreground
            .as_deref()
            .unwrap_or("black"),
        "selected_foreground",
    )?;
    let selected_background = parse_color(
        file_config
            .menu
            .selected_background
            .as_deref()
            .unwrap_or("cyan"),
        "selected_background",
    )?;
    let selected_bold = file_config.menu.selected_bold.unwrap_or(true);
    if title.trim().is_empty() {
        return Err(config_error("menu.title must be a non-empty string"));
    }
    if !one_character(&quit_key) {
        return Err(config_error("menu.quit_key must be one character"));
    }
    if key_format.matches("{key}").count() != 1
        || key_format.chars().count() > 32
        || key_format.chars().any(char::is_control)
    {
        return Err(config_error(
            "menu.key_format must contain {key} exactly once, use no control characters, and be at most 32 characters",
        ));
    }
    if file_config.items.is_empty() {
        return Err(config_error("at least one [[items]] table is required"));
    }
    if file_config.items.len() > MAX_MENU_ITEMS {
        return Err(config_error(format!(
            "menu contains {} items; maximum is {MAX_MENU_ITEMS}",
            file_config.items.len()
        )));
    }

    let mut used_keys = HashSet::from([
        normalize_key(&quit_key),
        "j".to_owned(),
        "k".to_owned(),
        "/".to_owned(),
    ]);
    let mut used_ids = HashSet::new();
    let mut items = Vec::with_capacity(file_config.items.len());
    for (index, item) in file_config.items.into_iter().enumerate() {
        let context = format!("items[{}]", index + 1);
        if item.label.trim().is_empty() {
            return Err(config_error(format!(
                "{context}.label must be a non-empty string"
            )));
        }
        match &item.command {
            CommandSpec::Shell(command) if command.trim().is_empty() => {
                return Err(config_error(format!("{context}.command must be non-empty")));
            }
            CommandSpec::Direct(parts)
                if parts.is_empty() || parts.iter().any(|part| part.is_empty()) =>
            {
                return Err(config_error(format!(
                    "{context}.command must be a non-empty array of non-empty strings"
                )));
            }
            _ => {}
        }
        if let Some(key) = &item.key {
            if !one_character(key) {
                return Err(config_error(format!("{context}.key must be one character")));
            }
            let normalized = normalize_key(key);
            if !used_keys.insert(normalized) {
                return Err(config_error(format!(
                    "duplicate or reserved key binding: '{key}'"
                )));
            }
        }
        if item
            .description
            .as_deref()
            .is_some_and(|description| description.trim().is_empty())
        {
            return Err(config_error(format!(
                "{context}.description must be non-empty when set"
            )));
        }
        if item
            .group
            .as_deref()
            .is_some_and(|group| group.trim().is_empty())
        {
            return Err(config_error(format!(
                "{context}.group must be non-empty when set"
            )));
        }
        if item.tags.iter().any(|tag| tag.trim().is_empty()) {
            return Err(config_error(format!(
                "{context}.tags must contain only non-empty strings"
            )));
        }
        let stable_id = match &item.id {
            Some(id) if valid_explicit_id(id) => format!("id:{id}"),
            Some(_) => {
                return Err(config_error(format!(
                    "{context}.id may contain only letters, numbers, '.', '_', and '-'"
                )));
            }
            None => stable_hash(&format!(
                "{}\u{1f}{}",
                item.label,
                command_display(&item.command)
            )),
        };
        if !used_ids.insert(stable_id.clone()) {
            return Err(config_error(format!(
                "{context} has a duplicate item identity; add unique id fields"
            )));
        }
        items.push(MenuItem {
            stable_id,
            label: item.label,
            command: item.command,
            key: item.key,
            confirm: item.confirm,
            description: item.description,
            group: item.group,
            tags: item.tags,
            favorite: item.favorite,
        });
    }
    Ok(MenuConfig {
        title,
        quit_key,
        loop_menu,
        key_format,
        selected_foreground,
        selected_background,
        selected_bold,
        items,
    })
}

fn resolve_config_path(
    explicit_path: Option<PathBuf>,
    local_path: &Path,
    system_path: &Path,
) -> Result<PathBuf, String> {
    if let Some(path) = explicit_path {
        return Ok(path);
    }
    if local_path.is_file() {
        return Ok(local_path.to_owned());
    }
    if system_path.is_file() {
        return Ok(system_path.to_owned());
    }
    Err(config_error(format!(
        "cannot find menu.toml in the current directory or {}",
        system_path.display()
    )))
}

fn load_config(path: &Path) -> Result<MenuConfig, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| config_error(format!("cannot read {}: {error}", path.display())))?;
    if text.len() > MAX_CONFIG_BYTES {
        return Err(config_error(format!(
            "configuration exceeds the {} byte limit",
            MAX_CONFIG_BYTES
        )));
    }
    parse_config_text(&text)
}

fn command_display(command: &CommandSpec) -> String {
    match command {
        CommandSpec::Shell(command) => command.clone(),
        CommandSpec::Direct(parts) => parts.join(" "),
    }
}

fn xdg_state_home() -> Option<PathBuf> {
    if let Some(value) = env::var_os("XDG_STATE_HOME") {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return Some(path);
        }
    }
    env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
}

impl RecentStore {
    fn load() -> Self {
        let path = xdg_state_home().map(|root| root.join("lazymenu-cli/recent-items"));
        let mut ids = Vec::new();
        if let Some(path) = &path
            && let Ok(contents) = fs::read_to_string(path)
        {
            let mut seen = HashSet::new();
            for id in contents.lines().map(str::trim).filter(|id| !id.is_empty()) {
                if seen.insert(id.to_owned()) {
                    ids.push(id.to_owned());
                }
                if ids.len() == MAX_RECENT_ITEMS {
                    break;
                }
            }
        }
        Self { path, ids }
    }

    fn rank(&self, stable_id: &str) -> Option<usize> {
        self.ids.iter().position(|id| id == stable_id)
    }

    fn contains(&self, stable_id: &str) -> bool {
        self.rank(stable_id).is_some()
    }

    fn mark_used(&mut self, stable_id: &str) -> io::Result<()> {
        self.ids.retain(|id| id != stable_id);
        self.ids.insert(0, stable_id.to_owned());
        self.ids.truncate(MAX_RECENT_ITEMS);
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension(format!("tmp-{}", process::id()));
        fs::write(&temporary, format!("{}\n", self.ids.join("\n")))?;
        fs::rename(temporary, path)
    }
}

fn fuzzy_score(query: &str, candidate: &str) -> Option<i64> {
    let query: Vec<char> = query.to_lowercase().chars().collect();
    let candidate: Vec<char> = candidate.to_lowercase().chars().collect();
    if query.is_empty() {
        return Some(0);
    }
    let mut score = 0_i64;
    let mut cursor = 0;
    let mut previous = None;
    for wanted in query {
        let offset = candidate[cursor..]
            .iter()
            .position(|character| *character == wanted)?;
        let index = cursor + offset;
        score += 20;
        if index == 0 {
            score += 20;
        } else if !candidate[index - 1].is_alphanumeric() {
            score += 12;
        }
        if previous.is_some_and(|last| index == last + 1) {
            score += 15;
        }
        score -= i64::try_from(offset).unwrap_or(i64::MAX).min(20);
        previous = Some(index);
        cursor = index + 1;
    }
    score -= i64::try_from(candidate.len()).unwrap_or(i64::MAX) / 8;
    Some(score)
}

fn item_match_score(item: &MenuItem, query: &str) -> Option<i64> {
    let mut total = 0_i64;
    for token in query.split_whitespace() {
        let mut best = fuzzy_score(token, &item.label).map(|score| score + 1_000);
        for (value, bonus) in [
            (item.group.as_deref(), 600),
            (item.description.as_deref(), 350),
            (item.key.as_deref(), 250),
        ] {
            if let Some(score) = value.and_then(|value| fuzzy_score(token, value)) {
                best = Some(best.map_or(score + bonus, |current| current.max(score + bonus)));
            }
        }
        for tag in &item.tags {
            if let Some(score) = fuzzy_score(token, tag) {
                best = Some(best.map_or(score + 550, |current| current.max(score + 550)));
            }
        }
        if let Some(score) = fuzzy_score(token, &command_display(&item.command)) {
            best = Some(best.map_or(score + 150, |current| current.max(score + 150)));
        }
        total += best?;
    }
    Some(total)
}

fn ordered_items(config: &MenuConfig, query: &str, recents: &RecentStore) -> Vec<usize> {
    if !query.trim().is_empty() {
        let mut matches: Vec<(usize, i64)> = config
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                item_match_score(item, query).map(|mut score| {
                    if item.favorite {
                        score += 100;
                    }
                    if let Some(rank) = recents.rank(&item.stable_id) {
                        score += i64::try_from(MAX_RECENT_ITEMS.saturating_sub(rank)).unwrap_or(0);
                    }
                    (index, score)
                })
            })
            .collect();
        matches.sort_by(|(left_index, left_score), (right_index, right_score)| {
            right_score.cmp(left_score).then_with(|| {
                config.items[*left_index]
                    .label
                    .cmp(&config.items[*right_index].label)
            })
        });
        return matches.into_iter().map(|(index, _)| index).collect();
    }

    let mut ordered = Vec::with_capacity(config.items.len());
    let mut included = vec![false; config.items.len()];
    for (index, item) in config.items.iter().enumerate() {
        if item.favorite {
            ordered.push(index);
            included[index] = true;
        }
    }
    for recent_id in &recents.ids {
        if let Some((index, _)) = config
            .items
            .iter()
            .enumerate()
            .find(|(index, item)| !included[*index] && &item.stable_id == recent_id)
        {
            ordered.push(index);
            included[index] = true;
        }
    }
    let mut remaining: Vec<usize> = (0..config.items.len())
        .filter(|index| !included[*index])
        .collect();
    remaining.sort_by(|left, right| {
        let left_item = &config.items[*left];
        let right_item = &config.items[*right];
        left_item
            .group
            .as_deref()
            .unwrap_or("Other")
            .to_lowercase()
            .cmp(
                &right_item
                    .group
                    .as_deref()
                    .unwrap_or("Other")
                    .to_lowercase(),
            )
            .then_with(|| left.cmp(right))
    });
    ordered.extend(remaining);
    ordered
}

fn truncate_text(value: &str, width: usize) -> String {
    let length = value.chars().count();
    if length <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let mut shortened: String = value.chars().take(width - 1).collect();
    shortened.push('…');
    shortened
}

fn colors_enabled() -> bool {
    io::stdout().is_terminal()
        && env::var_os("NO_COLOR").is_none()
        && env::var("TERM").is_ok_and(|term| term != "dumb")
}

fn reset_style(output: &mut impl Write, colors: bool) -> io::Result<()> {
    if colors {
        queue!(output, ResetColor, SetAttribute(Attribute::Reset))?;
    }
    Ok(())
}

fn render(
    config: &MenuConfig,
    ordered: &[usize],
    selected: usize,
    search_mode: bool,
    query: &str,
    recents: &RecentStore,
    message: Option<(&str, bool)>,
) -> io::Result<()> {
    let colors = colors_enabled();
    let (terminal_columns, terminal_rows) = terminal::size().unwrap_or((80, 24));
    let terminal_columns = usize::from(terminal_columns.max(20));
    let terminal_rows = terminal_rows.max(10);
    let visible_capacity = usize::from(terminal_rows.saturating_sub(8)).max(1);
    let visible_count = ordered.len().min(visible_capacity);
    let binding_width = config
        .items
        .iter()
        .filter_map(|item| item.key.as_deref())
        .map(|key| format_binding(&config.key_format, key).chars().count())
        .max()
        .unwrap_or(0);
    let maximum_start = ordered.len().saturating_sub(visible_count);
    let first_visible = selected
        .saturating_sub(visible_count / 2)
        .min(maximum_start);
    let last_visible = first_visible + visible_count;
    let mut output = io::stdout();
    queue!(output, Clear(ClearType::All), MoveTo(0, 0), Hide)?;
    if colors {
        queue!(
            output,
            SetForegroundColor(Color::Cyan),
            SetAttribute(Attribute::Bold)
        )?;
    }
    write!(output, "{}", truncate_text(&config.title, terminal_columns))?;
    reset_style(&mut output, colors)?;
    queue!(output, MoveTo(0, 1))?;
    if colors {
        queue!(output, SetAttribute(Attribute::Dim))?;
    }
    write!(
        output,
        "{}",
        "─".repeat(config.title.chars().count().max(12).min(terminal_columns))
    )?;
    reset_style(&mut output, colors)?;

    queue!(output, MoveTo(0, 2))?;
    if colors {
        queue!(output, SetAttribute(Attribute::Dim))?;
    }
    if search_mode {
        write!(
            output,
            "{}",
            truncate_text(&format!("Search: {query}_"), terminal_columns)
        )?;
    } else {
        write!(output, "Press / to search")?;
    }
    reset_style(&mut output, colors)?;

    for (visible_index, item_index) in ordered[first_visible..last_visible].iter().enumerate() {
        let item = &config.items[*item_index];
        let position = first_visible + visible_index;
        queue!(
            output,
            MoveTo(0, u16::try_from(visible_index + 4).unwrap_or(u16::MAX))
        )?;
        if position == selected && colors {
            queue!(
                output,
                SetForegroundColor(config.selected_foreground),
                SetBackgroundColor(config.selected_background)
            )?;
            if config.selected_bold {
                queue!(output, SetAttribute(Attribute::Bold))?;
            }
        }
        let marker = if position == selected { "›" } else { " " };
        let badge = if item.favorite {
            "★"
        } else if recents.contains(&item.stable_id) {
            "↻"
        } else {
            " "
        };
        let binding = item
            .key
            .as_deref()
            .map(|key| format_binding(&config.key_format, key))
            .unwrap_or_default();
        let binding_column = if binding_width == 0 {
            String::new()
        } else {
            format!("{binding:<binding_width$} ")
        };
        let mut line = format!("{marker} {badge} {binding_column}{}", item.label);
        if let Some(group) = &item.group {
            line.push_str("  · ");
            line.push_str(group);
        }
        line = truncate_text(&line, terminal_columns);
        if position == selected {
            line.extend(std::iter::repeat_n(
                ' ',
                terminal_columns.saturating_sub(line.chars().count()),
            ));
        }
        write!(output, "{line}")?;
        reset_style(&mut output, colors)?;
    }
    if ordered.is_empty() {
        queue!(output, MoveTo(0, 4))?;
        if colors {
            queue!(output, SetAttribute(Attribute::Dim))?;
        }
        write!(output, "No matching commands")?;
        reset_style(&mut output, colors)?;
    }

    let description_row = terminal_rows.saturating_sub(3);
    let help_row = terminal_rows.saturating_sub(2);
    let status_row = terminal_rows.saturating_sub(1);
    queue!(output, MoveTo(0, description_row))?;
    if let Some(item_index) = ordered.get(selected) {
        let item = &config.items[*item_index];
        let description = item
            .description
            .as_deref()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Command: {}", command_display(&item.command)));
        if colors {
            queue!(output, SetAttribute(Attribute::Dim))?;
        }
        write!(output, "{}", truncate_text(&description, terminal_columns))?;
        reset_style(&mut output, colors)?;
    }

    queue!(output, MoveTo(0, help_row))?;
    if colors {
        queue!(output, SetAttribute(Attribute::Dim))?;
    }
    let help = if search_mode {
        "Type to filter • ↑/↓ select • PgUp/PgDn • Enter run • Esc clear/close"
    } else {
        "↑/↓ or j/k • PgUp/PgDn • Home/End • / search • Enter run • q/Esc quit"
    };
    write!(output, "{}", truncate_text(help, terminal_columns))?;
    reset_style(&mut output, colors)?;

    queue!(output, MoveTo(0, status_row))?;
    if let Some((text, failed)) = message {
        if colors {
            if failed {
                queue!(output, SetForegroundColor(Color::Red))?;
            } else {
                queue!(output, SetAttribute(Attribute::Dim))?;
            }
        }
        write!(output, "{}", truncate_text(text, terminal_columns))?;
        reset_style(&mut output, colors)?;
    } else {
        if colors {
            queue!(output, SetAttribute(Attribute::Dim))?;
        }
        let noun = if search_mode { "matches" } else { "commands" };
        let status = if ordered.is_empty() {
            format!("0 {noun}")
        } else {
            format!(
                "{} {noun} • items {}–{}/{}",
                ordered.len(),
                first_visible + 1,
                last_visible,
                ordered.len()
            )
        };
        write!(output, "{}", truncate_text(&status, terminal_columns))?;
        reset_style(&mut output, colors)?;
    }
    output.flush()
}

fn next_key() -> io::Result<KeyEvent> {
    loop {
        if let Event::Key(key) = event::read()?
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        {
            return Ok(key);
        }
    }
}

fn ctrl_c(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn key_text(key: KeyEvent) -> Option<String> {
    match key.code {
        KeyCode::Char(character) => Some(character.to_string().to_lowercase()),
        _ => None,
    }
}

fn key_character(key: KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(character)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            Some(character)
        }
        _ => None,
    }
}

fn page_size() -> usize {
    terminal::size()
        .map(|(_, rows)| usize::from(rows.max(10).saturating_sub(8)).max(1))
        .unwrap_or(16)
}

fn run_command(item: &MenuItem, dry_run: bool, interrupted: &AtomicBool) -> i32 {
    let display = command_display(&item.command);
    if dry_run {
        println!("would run: {display}");
        return 0;
    }
    println!("\nRunning: {display}\n");
    let mut command = match &item.command {
        CommandSpec::Direct(parts) => {
            let mut command = Command::new(&parts[0]);
            command.args(&parts[1..]);
            command
        }
        CommandSpec::Shell(script) => {
            #[cfg(windows)]
            let command = {
                let mut command = Command::new("cmd");
                command.args(["/C", script]);
                command
            };
            #[cfg(not(windows))]
            let command = {
                let mut command = Command::new("sh");
                command.args(["-c", script]);
                command
            };
            command
        }
    };
    match command.status() {
        Ok(status) => status
            .code()
            .unwrap_or(if interrupted.load(Ordering::SeqCst) {
                EXIT_INTERRUPTED
            } else {
                1
            }),
        Err(error) => {
            eprintln!("Could not start command: {error}");
            EXIT_COMMAND_NOT_FOUND
        }
    }
}

fn interactive(config: MenuConfig, dry_run: bool) -> Result<i32, String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(config_error(
            "interactive mode needs a terminal; use --print for scripts",
        ));
    }
    let running = Arc::new(AtomicBool::new(false));
    let interrupted = Arc::new(AtomicBool::new(false));
    let signal_running = Arc::clone(&running);
    let signal_interrupted = Arc::clone(&interrupted);
    ctrlc::set_handler(move || {
        if signal_running.load(Ordering::SeqCst) {
            signal_interrupted.store(true, Ordering::SeqCst);
        }
    })
    .map_err(|error| config_error(format!("cannot install Ctrl+C handler: {error}")))?;

    let mut terminal = TerminalGuard::new();
    terminal
        .enter()
        .map_err(|error| config_error(format!("cannot configure terminal: {error}")))?;
    let mut recents = RecentStore::load();
    let mut selected = 0_usize;
    let mut search_mode = false;
    let mut query = String::new();
    let mut message: Option<(String, bool)> = None;

    loop {
        let ordered = ordered_items(&config, &query, &recents);
        selected = selected.min(ordered.len().saturating_sub(1));
        render(
            &config,
            &ordered,
            selected,
            search_mode,
            &query,
            &recents,
            message
                .as_ref()
                .map(|(text, failed)| (text.as_str(), *failed)),
        )
        .map_err(|error| config_error(format!("cannot render menu: {error}")))?;
        message = None;

        let key = next_key().map_err(|error| config_error(format!("cannot read key: {error}")))?;
        if ctrl_c(key) {
            return Ok(EXIT_INTERRUPTED);
        }

        let mut activate = None;
        if search_mode {
            match key.code {
                KeyCode::Esc if query.is_empty() => {
                    search_mode = false;
                    selected = 0;
                }
                KeyCode::Esc => {
                    query.clear();
                    selected = 0;
                }
                KeyCode::Backspace => {
                    query.pop();
                    selected = 0;
                }
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    if selected + 1 < ordered.len() {
                        selected += 1;
                    }
                }
                KeyCode::PageUp => {
                    selected = selected.saturating_sub(page_size());
                }
                KeyCode::PageDown => {
                    selected = (selected + page_size()).min(ordered.len().saturating_sub(1));
                }
                KeyCode::Home => selected = 0,
                KeyCode::End => selected = ordered.len().saturating_sub(1),
                KeyCode::Enter => activate = ordered.get(selected).copied(),
                _ => {
                    if let Some(character) = key_character(key) {
                        query.push(character);
                        selected = 0;
                    }
                }
            }
        } else {
            let text = key_text(key);
            if key.code == KeyCode::Esc
                || text.as_deref() == Some(normalize_key(&config.quit_key).as_str())
            {
                return Ok(0);
            }
            match key.code {
                KeyCode::Char('/') => {
                    search_mode = true;
                    query.clear();
                    selected = 0;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if selected + 1 < ordered.len() {
                        selected += 1;
                    }
                }
                KeyCode::PageUp => selected = selected.saturating_sub(page_size()),
                KeyCode::PageDown => {
                    selected = (selected + page_size()).min(ordered.len().saturating_sub(1));
                }
                KeyCode::Home => selected = 0,
                KeyCode::End => selected = ordered.len().saturating_sub(1),
                KeyCode::Enter => activate = ordered.get(selected).copied(),
                _ => {
                    activate = text.as_deref().and_then(|pressed| {
                        config.items.iter().position(|item| {
                            item.key
                                .as_deref()
                                .is_some_and(|binding| normalize_key(binding) == pressed)
                        })
                    });
                    if let Some(item_index) = activate {
                        selected = ordered
                            .iter()
                            .position(|index| *index == item_index)
                            .unwrap_or(0);
                    } else {
                        message = Some(("Unknown key. Press / to search.".to_owned(), true));
                    }
                }
            }
        }

        let Some(item_index) = activate else {
            continue;
        };
        let item = &config.items[item_index];
        render(
            &config,
            &ordered,
            selected,
            search_mode,
            &query,
            &recents,
            None,
        )
        .map_err(|error| config_error(format!("cannot render menu: {error}")))?;
        if item.confirm {
            let row = terminal::size()
                .map(|(_, rows)| rows.saturating_sub(1))
                .unwrap_or(23);
            execute!(io::stdout(), MoveTo(0, row), Clear(ClearType::CurrentLine))
                .map_err(|error| config_error(error.to_string()))?;
            print!("Run '{}'? [y/N] ", item.label);
            io::stdout()
                .flush()
                .map_err(|error| config_error(error.to_string()))?;
            let answer = next_key().map_err(|error| config_error(error.to_string()))?;
            if ctrl_c(answer) {
                return Ok(EXIT_INTERRUPTED);
            }
            if key_text(answer).as_deref() != Some("y") {
                message = Some(("Cancelled.".to_owned(), false));
                continue;
            }
        }

        terminal
            .leave()
            .map_err(|error| config_error(format!("cannot restore terminal: {error}")))?;
        interrupted.store(false, Ordering::SeqCst);
        running.store(true, Ordering::SeqCst);
        let status = run_command(item, dry_run, &interrupted);
        running.store(false, Ordering::SeqCst);
        let was_interrupted =
            interrupted.swap(false, Ordering::SeqCst) || status == EXIT_INTERRUPTED;
        let state_error = if dry_run {
            None
        } else {
            recents.mark_used(&item.stable_id).err()
        };
        if !config.loop_menu {
            if let Some(error) = state_error {
                eprintln!("Command completed; recent state was not saved: {error}");
            }
            return Ok(if was_interrupted {
                EXIT_INTERRUPTED
            } else {
                status
            });
        }
        terminal
            .enter()
            .map_err(|error| config_error(format!("cannot configure terminal: {error}")))?;
        let refreshed = ordered_items(&config, &query, &recents);
        selected = refreshed
            .iter()
            .position(|index| *index == item_index)
            .unwrap_or(0);
        if was_interrupted {
            message = Some((format!("'{}' interrupted.", item.label), true));
            continue;
        }
        if status != 0 {
            message = Some((
                format!("'{}' failed with status {status}.", item.label),
                true,
            ));
            continue;
        }
        let row = terminal::size()
            .map(|(_, rows)| rows.saturating_sub(1))
            .unwrap_or(23);
        execute!(io::stdout(), MoveTo(0, row), Clear(ClearType::CurrentLine))
            .map_err(|error| config_error(error.to_string()))?;
        print!("Press any key to return to the menu...");
        io::stdout()
            .flush()
            .map_err(|error| config_error(error.to_string()))?;
        let answer = next_key().map_err(|error| config_error(error.to_string()))?;
        if ctrl_c(answer) {
            return Ok(EXIT_INTERRUPTED);
        }
        if answer.code == KeyCode::Esc {
            return Ok(0);
        }
        message = Some(match state_error {
            Some(error) => (
                format!("Command completed; recent state was not saved: {error}"),
                true,
            ),
            None => (format!("'{}' completed.", item.label), false),
        });
    }
}

fn main() {
    let result = (|| -> Result<i32, String> {
        let options = parse_options()?;
        let config_path = resolve_config_path(
            options.config_path,
            Path::new("menu.toml"),
            Path::new(SYSTEM_CONFIG_PATH),
        )?;
        let config = load_config(&config_path)?;
        if options.print_items {
            for item in &config.items {
                println!(
                    "{}\t{}\t{}",
                    item.key.as_deref().unwrap_or("-"),
                    item.label,
                    command_display(&item.command)
                );
            }
            Ok(0)
        } else {
            interactive(config, options.dry_run)
        }
    })();
    match result {
        Ok(status) => process::exit(status),
        Err(error) => {
            eprintln!("{error}");
            process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn loads_the_default_menu() {
        let config = parse_config_text(include_str!("../menu.toml")).expect("default config loads");
        assert_eq!(config.title, "Workspace tools");
        assert!(config.loop_menu);
        assert_eq!(config.items.len(), 4);
    }

    #[test]
    fn project_menu_takes_priority_over_the_system_menu() {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let test_root = env::temp_dir().join(format!(
            "lazymenu-cli-config-test-{}-{unique_suffix}",
            process::id()
        ));
        fs::create_dir_all(&test_root).expect("create test directory");
        let local_path = test_root.join("menu.toml");
        let system_path = test_root.join("system-menu.toml");
        fs::write(&system_path, "[menu]\n").expect("write system menu");

        assert_eq!(
            resolve_config_path(None, &local_path, &system_path).expect("system fallback"),
            system_path
        );

        fs::write(&local_path, "[menu]\n").expect("write project menu");
        assert_eq!(
            resolve_config_path(None, &local_path, &system_path).expect("project menu"),
            local_path
        );
        fs::remove_dir_all(test_root).expect("remove test directory");
    }

    #[test]
    fn explicit_config_path_takes_priority_over_existing_menus() {
        let explicit_path = PathBuf::from("custom-menu.toml");
        assert_eq!(
            resolve_config_path(
                Some(explicit_path.clone()),
                Path::new("menu.toml"),
                Path::new(SYSTEM_CONFIG_PATH),
            )
            .expect("explicit path"),
            explicit_path
        );
    }

    #[test]
    fn menu_loop_defaults_on_and_can_be_disabled() {
        let default_config = parse_config_text(
            r#"
                [[items]]
                label = "One"
                command = "echo one"
            "#,
        )
        .expect("default loop behavior loads");
        assert!(default_config.loop_menu);

        let one_shot_config = parse_config_text(
            r#"
                [menu]
                loop = false

                [[items]]
                label = "One"
                command = "echo one"
            "#,
        )
        .expect("one-shot behavior loads");
        assert!(!one_shot_config.loop_menu);
    }

    #[test]
    fn rejects_reserved_bindings() {
        let result = parse_config_text(
            r#"
                [[items]]
                label = "One"
                key = "q"
                command = "echo one"
            "#,
        );
        assert!(result.unwrap_err().contains("reserved key binding"));
    }

    #[test]
    fn loads_custom_menu_presentation() {
        let config = parse_config_text(
            r##"
                [menu]
                key_format = "{key}:"
                selected_foreground = "white"
                selected_background = "#123456"
                selected_bold = false

                [[items]]
                label = "One"
                key = "1"
                command = "echo one"
            "##,
        )
        .expect("custom presentation loads");
        assert_eq!(format_binding(&config.key_format, "1"), "1:");
        assert_eq!(config.selected_foreground, Color::White);
        assert_eq!(
            config.selected_background,
            Color::Rgb {
                r: 0x12,
                g: 0x34,
                b: 0x56
            }
        );
        assert!(!config.selected_bold);
    }

    #[test]
    fn rejects_invalid_menu_presentation() {
        let missing_placeholder = parse_config_text(
            r#"
                [menu]
                key_format = "key:"

                [[items]]
                label = "One"
                command = "echo one"
            "#,
        );
        assert!(missing_placeholder.unwrap_err().contains("key_format"));

        let invalid_color = parse_config_text(
            r#"
                [menu]
                selected_background = "invisible"

                [[items]]
                label = "One"
                command = "echo one"
            "#,
        );
        assert!(invalid_color.unwrap_err().contains("selected_background"));
    }

    #[test]
    fn fuzzy_search_matches_metadata() {
        let config = parse_config_text(
            r#"
                [[items]]
                id = "deploy-prod"
                label = "Publish release"
                command = ["deploy", "--production"]
                group = "Deployment"
                tags = ["ship", "production"]
                description = "Deploy the current release"
                favorite = true
            "#,
        )
        .expect("config loads");
        assert!(item_match_score(&config.items[0], "dply prod").is_some());
        assert!(config.items[0].favorite);
    }

    #[test]
    fn missing_executable_returns_command_not_found_status() {
        let item = MenuItem {
            stable_id: "id:missing".to_owned(),
            label: "Missing".to_owned(),
            command: CommandSpec::Direct(vec![
                "lazymenu-cli-command-that-does-not-exist".to_owned(),
            ]),
            key: None,
            confirm: false,
            description: None,
            group: None,
            tags: Vec::new(),
            favorite: false,
        };
        assert_eq!(
            run_command(&item, false, &AtomicBool::new(false)),
            EXIT_COMMAND_NOT_FOUND
        );
    }

    #[test]
    fn favorites_and_recents_are_ranked_first() {
        let config = parse_config_text(include_str!("../menu.toml")).expect("default config loads");
        let recents = RecentStore {
            path: None,
            ids: vec!["id:show-date".to_owned()],
        };
        let ordered = ordered_items(&config, "", &recents);
        assert_eq!(config.items[ordered[0]].stable_id, "id:show-directory");
        assert_eq!(config.items[ordered[1]].stable_id, "id:show-date");
    }

    #[test]
    fn rejects_more_than_the_item_limit() {
        let mut config = String::new();
        for index in 0..=MAX_MENU_ITEMS {
            config.push_str(&format!(
                "[[items]]\nlabel = \"Item {index}\"\ncommand = [\"true\"]\n"
            ));
        }
        assert!(
            parse_config_text(&config)
                .unwrap_err()
                .contains("maximum is 1000")
        );
    }
}
