//! Icons bundled into the binary. Paths are adapted from Lucide (ISC license).

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Activity,
    Alert,
    ArrowDown,
    ArrowUp,
    Check,
    CheckCircle,
    ChevronDown,
    ChevronRight,
    Clock,
    Copy,
    External,
    Info,
    Link,
    Loader,
    Play,
    Plug,
    Plus,
    Power,
    User,
    X,
}

const ALL: &[Icon] = &[
    Icon::Activity,
    Icon::Alert,
    Icon::ArrowDown,
    Icon::ArrowUp,
    Icon::Check,
    Icon::CheckCircle,
    Icon::ChevronDown,
    Icon::ChevronRight,
    Icon::Clock,
    Icon::Copy,
    Icon::External,
    Icon::Info,
    Icon::Link,
    Icon::Loader,
    Icon::Play,
    Icon::Plug,
    Icon::Plus,
    Icon::Power,
    Icon::User,
    Icon::X,
];

impl Icon {
    pub fn path(self) -> &'static str {
        match self {
            Self::Activity => "icons/activity.svg",
            Self::Alert => "icons/alert.svg",
            Self::ArrowDown => "icons/arrow-down.svg",
            Self::ArrowUp => "icons/arrow-up.svg",
            Self::Check => "icons/check.svg",
            Self::CheckCircle => "icons/check-circle.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::Clock => "icons/clock.svg",
            Self::Copy => "icons/copy.svg",
            Self::External => "icons/external.svg",
            Self::Info => "icons/info.svg",
            Self::Link => "icons/link.svg",
            Self::Loader => "icons/loader.svg",
            Self::Play => "icons/play.svg",
            Self::Plug => "icons/plug.svg",
            Self::Plus => "icons/plus.svg",
            Self::Power => "icons/power.svg",
            Self::User => "icons/user.svg",
            Self::X => "icons/x.svg",
        }
    }

    fn shapes(self) -> &'static str {
        match self {
            Self::Activity => r#"<path d="M22 12h-4l-3 9L9 3l-3 9H2"/>"#,
            Self::Alert => {
                r#"<path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"/><path d="M12 9v4"/><path d="M12 17h.01"/>"#
            }
            Self::ArrowDown => r#"<path d="M12 5v14"/><path d="m19 12-7 7-7-7"/>"#,
            Self::ArrowUp => r#"<path d="m5 12 7-7 7 7"/><path d="M12 19V5"/>"#,
            Self::Check => r#"<path d="M20 6 9 17l-5-5"/>"#,
            Self::CheckCircle => r#"<circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/>"#,
            Self::ChevronDown => r#"<path d="m6 9 6 6 6-6"/>"#,
            Self::ChevronRight => r#"<path d="m9 18 6-6-6-6"/>"#,
            Self::Clock => r#"<circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/>"#,
            Self::Copy => {
                r#"<rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>"#
            }
            Self::External => {
                r#"<path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>"#
            }
            Self::Info => {
                r#"<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>"#
            }
            Self::Link => {
                r#"<path d="M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7"/><path d="M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7"/>"#
            }
            Self::Loader => r#"<path d="M21 12a9 9 0 1 1-6.2-8.6"/>"#,
            Self::Play => r#"<path d="M8 5.5v13l10.5-6.5z" fill="black"/>"#,
            Self::Plug => {
                r#"<path d="M12 22v-5"/><path d="M9 8V2"/><path d="M15 8V2"/><path d="M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8Z"/>"#
            }
            Self::Plus => r#"<path d="M5 12h14"/><path d="M12 5v14"/>"#,
            Self::Power => r#"<path d="M12 2v10"/><path d="M18.4 6.6a9 9 0 1 1-12.8 0"/>"#,
            Self::User => r#"<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>"#,
            Self::X => r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#,
        }
    }
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(ALL.iter().find(|icon| icon.path() == path).map(|icon| {
            Cow::Owned(
                format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
                    icon.shapes()
                )
                .into_bytes(),
            )
        }))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ALL
            .iter()
            .map(|icon| icon.path())
            .filter(|icon_path| icon_path.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}
