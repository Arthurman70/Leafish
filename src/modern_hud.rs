//! Native-session HUD using the installed modern hotbar sprites.
//! Unsupported item models retain explicit text labels.

use crate::ui::{self, HAttach, VAttach};

pub const PICKER_ROWS: usize = 10;

/// The session owns filtering, paging, selection and all input. Entries contain
/// the visible page; `selected` is an index within that page.
pub struct PickerView<'a> {
    pub query: &'a str,
    pub entries: &'a [String],
    pub selected: usize,
    pub offset: usize,
    pub total: usize,
}

pub struct Hud {
    _bar: ui::ImageRef,
    selection: ui::ImageRef,
    _numbers: Vec<ui::TextRef>,
    labels: Vec<ui::TextRef>,
    icons: Vec<ui::ImageRef>,
    selected_name: ui::TextRef,
    status: ui::TextRef,
    _help: ui::TextRef,
    panel: ui::ImageRef,
    panel_selection: ui::ImageRef,
    panel_title: ui::TextRef,
    panel_query: ui::TextRef,
    panel_help: ui::TextRef,
    panel_rows: Vec<ui::TextRef>,
    panel_footer: ui::TextRef,
}

impl Hud {
    pub fn new(container: &mut ui::Container) -> Self {
        let bar = ui::ImageBuilder::new()
            .texture("minecraft:gui/sprites/hud/hotbar")
            .size(364.0, 44.0)
            .position(0.0, 8.0)
            .alignment(VAttach::Bottom, HAttach::Center)
            .draw_index(10)
            .create(container);
        let selection = ui::ImageBuilder::new()
            .texture("minecraft:gui/sprites/hud/hotbar_selection")
            .size(48.0, 46.0)
            .position(-160.0, 8.0)
            .alignment(VAttach::Bottom, HAttach::Center)
            .draw_index(11)
            .create(container);
        let mut numbers = Vec::with_capacity(9);
        let mut labels = Vec::with_capacity(9);
        let mut icons = Vec::with_capacity(9);
        for slot in 0..9 {
            let x = -160.0 + slot as f64 * 40.0;
            numbers.push(
                ui::TextBuilder::new()
                    .text((slot + 1).to_string())
                    .position(x, 35.0)
                    .scale_x(0.65)
                    .scale_y(0.65)
                    .colour((205, 205, 205, 255))
                    .alignment(VAttach::Bottom, HAttach::Center)
                    .draw_index(12)
                    .create(container),
            );
            labels.push(
                ui::TextBuilder::new()
                    .text("...")
                    .position(x, 14.0)
                    .scale_x(0.75)
                    .scale_y(0.75)
                    .alignment(VAttach::Bottom, HAttach::Center)
                    .draw_index(12)
                    .create(container),
            );
            icons.push(
                ui::ImageBuilder::new()
                    .texture("leafish:solid")
                    .size(30.0, 30.0)
                    .position(x, 11.0)
                    .colour((255, 255, 255, 0))
                    .alignment(VAttach::Bottom, HAttach::Center)
                    .draw_index(12)
                    .create(container),
            );
        }
        let selected_name = ui::TextBuilder::new()
            .text("Waiting for inventory...")
            .position(0.0, 64.0)
            .alignment(VAttach::Bottom, HAttach::Center)
            .draw_index(12)
            .create(container);
        let status = ui::TextBuilder::new()
            .text("Loading Minecraft 1.21.1...")
            .position(12.0, 12.0)
            .draw_index(12)
            .create(container);
        let help = ui::TextBuilder::new()
            .text("1-9 / wheel: select | E: blocks | Esc: pause")
            .position(12.0, 34.0)
            .scale_x(0.8)
            .scale_y(0.8)
            .colour((215, 215, 215, 255))
            .draw_index(12)
            .create(container);

        let panel = ui::ImageBuilder::new()
            .texture("leafish:solid")
            .size(600.0, 344.0)
            .colour((20, 22, 25, 0))
            .alignment(VAttach::Middle, HAttach::Center)
            .draw_index(30)
            .create(container);
        let mut parent = panel.borrow_mut();
        let panel_title = ui::TextBuilder::new()
            .text("Creative blocks")
            .position(0.0, 14.0)
            .colour((255, 255, 255, 0))
            .alignment(VAttach::Top, HAttach::Center)
            .draw_index(2)
            .attach(&mut *parent);
        let panel_query = ui::TextBuilder::new()
            .text("Search: ")
            .position(18.0, 46.0)
            .colour((255, 255, 255, 0))
            .draw_index(2)
            .attach(&mut *parent);
        let panel_help = ui::TextBuilder::new()
            .text("Type to search | Up/Down: choose | Enter: use | Esc: close")
            .position(18.0, 72.0)
            .scale_x(0.75)
            .scale_y(0.75)
            .colour((190, 190, 190, 0))
            .draw_index(2)
            .attach(&mut *parent);
        let panel_selection = ui::ImageBuilder::new()
            .texture("leafish:solid")
            .size(564.0, 20.0)
            .position(0.0, 99.0)
            .colour((100, 130, 170, 0))
            .alignment(VAttach::Top, HAttach::Center)
            .draw_index(1)
            .attach(&mut *parent);
        let mut panel_rows = Vec::with_capacity(PICKER_ROWS);
        for row in 0..PICKER_ROWS {
            panel_rows.push(
                ui::TextBuilder::new()
                    .text("")
                    .position(26.0, 101.0 + row as f64 * 20.0)
                    .scale_x(0.9)
                    .scale_y(0.9)
                    .colour((235, 235, 235, 0))
                    .draw_index(2)
                    .attach(&mut *parent),
            );
        }
        let panel_footer = ui::TextBuilder::new()
            .text("")
            .position(0.0, 318.0)
            .scale_x(0.75)
            .scale_y(0.75)
            .colour((190, 190, 190, 0))
            .alignment(VAttach::Top, HAttach::Center)
            .draw_index(2)
            .attach(&mut *parent);
        drop(parent);
        Self {
            _bar: bar,
            selection,
            _numbers: numbers,
            labels,
            icons,
            selected_name,
            status,
            _help: help,
            panel,
            panel_selection,
            panel_title,
            panel_query,
            panel_help,
            panel_rows,
            panel_footer,
        }
    }

    pub fn update(
        &mut self,
        hotbar: [Option<&str>; 9],
        icons: [Option<&str>; 9],
        selected: usize,
        status: &str,
        picker: Option<PickerView<'_>>,
    ) {
        let selected = selected.min(8);
        self.selection.borrow_mut().x = -160.0 + selected as f64 * 40.0;
        for (slot, label) in self.labels.iter().enumerate() {
            let mut label = label.borrow_mut();
            label.text = hotbar[slot].map_or_else(|| "...".to_owned(), short_label);
            label.colour = if slot == selected {
                (255, 245, 175, 255)
            } else {
                (245, 245, 245, 255)
            };
            let mut icon = self.icons[slot].borrow_mut();
            if let Some(texture) = icons[slot] {
                icon.texture = texture.to_owned();
                icon.colour.3 = 255;
                label.colour.3 = 0;
            } else {
                icon.colour.3 = 0;
            }
        }
        self.selected_name.borrow_mut().text = hotbar[selected]
            .map(display_name)
            .unwrap_or_else(|| "Waiting for inventory...".into());
        self.status.borrow_mut().text = status.to_owned();
        let alpha = if picker.is_some() { 255 } else { 0 };
        self.panel.borrow_mut().colour.3 = if picker.is_some() { 235 } else { 0 };
        for label in [
            &self.panel_title,
            &self.panel_query,
            &self.panel_help,
            &self.panel_footer,
        ] {
            label.borrow_mut().colour.3 = alpha;
        }
        if let Some(picker) = picker {
            let visible = picker.entries.len().min(PICKER_ROWS);
            self.panel_query.borrow_mut().text = format!("Search: {}_", picker.query);
            self.panel_selection.borrow_mut().colour.3 = if visible > 0 { 180 } else { 0 };
            self.panel_selection.borrow_mut().y =
                99.0 + picker.selected.min(visible.saturating_sub(1)) as f64 * 20.0;
            for (row, label) in self.panel_rows.iter().enumerate() {
                let mut label = label.borrow_mut();
                label.text = picker
                    .entries
                    .get(row)
                    .map_or_else(String::new, |name| display_name(name));
                label.colour.3 = if row < visible { 255 } else { 0 };
            }
            self.panel_footer.borrow_mut().text = if visible == 0 {
                "No matching blocks".into()
            } else {
                format!(
                    "{}-{} of {} | Replaces hotbar slot {}",
                    picker.offset + 1,
                    picker.offset + visible,
                    picker.total,
                    selected + 1
                )
            };
        } else {
            self.panel_selection.borrow_mut().colour.3 = 0;
            for label in &self.panel_rows {
                label.borrow_mut().colour.3 = 0;
            }
        }
    }
}

fn display_name(name: &str) -> String {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    name.split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn short_label(name: &str) -> String {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    let words: Vec<_> = name.split('_').collect();
    if words.len() == 1 {
        return display_name(name).chars().take(4).collect();
    }
    let per_word = if words.len() == 2 { 2 } else { 1 };
    words
        .iter()
        .take(4)
        .map(|word| {
            display_name(word)
                .chars()
                .take(per_word)
                .collect::<String>()
        })
        .collect()
}
