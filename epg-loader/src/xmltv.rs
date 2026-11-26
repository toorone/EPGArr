//! Streaming XMLTV handling with quick-xml.
//!
//! Two passes over the same bytes:
//!  * `scan` collects what each channel and programme already has;
//!  * `rewrite` copies every event through unchanged and appends the planned
//!    additions just before each </channel> / </programme>.
//!
//! Programmes are matched between the passes by their position, so both
//! passes must count top-level <programme> elements the same way.

use anyhow::{anyhow, Result};
use quick_xml::events::{BytesCData, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use std::collections::HashMap;
use std::io::Write;

#[derive(Debug, Default, Clone)]
pub struct ChannelInfo {
    pub id: String,
    pub names: Vec<String>,
    /// An <icon> with a non-empty src is present.
    pub has_icon: bool,
    /// The first such icon (for the guide page).
    pub icon: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct ProgInfo {
    pub channel: String,
    pub start: String,
    pub stop: String,
    pub title: Option<String>,
    pub sub_title: Option<String>,
    /// First <episode-num>, shown as S01E05 when it is in xmltv_ns form.
    pub episode: Option<String>,
    pub desc: Option<String>,
    pub icon: Option<String>,
    /// First <rating><value>.
    pub rating: Option<String>,
    pub date: Option<String>,
    /// Has an <episode-num>, so it is treated as TV.
    pub is_tv: bool,
    pub categories: Vec<String>,
    pub has_desc: bool,
    pub has_icon: bool,
    pub has_rating: bool,
    /// Has a <star-rating> already (an IMDb one is then not added).
    pub has_star: bool,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub channels: Vec<ChannelInfo>,
    pub programmes: Vec<ProgInfo>,
}

/// What to add to one programme. Only filled for things that are missing.
#[derive(Debug, Default, Clone)]
pub struct ProgPlan {
    pub drop: bool,
    pub desc: Option<String>,
    pub category: Option<String>,
    pub icon: Option<String>,
    /// (system, value)
    pub rating: Option<(String, String)>,
    /// (system, value) for a <star-rating>, e.g. ("IMDb", "7.8/10").
    pub star: Option<(String, String)>,
}

impl ProgPlan {
    fn has_additions(&self) -> bool {
        self.desc.is_some() || self.category.is_some() || self.icon.is_some() || self.rating.is_some() || self.star.is_some()
    }
}

enum Top {
    Other,
    Channel(ChannelInfo),
    Programme(ProgInfo),
}

fn xml_err(e: quick_xml::Error, pos: u64) -> anyhow::Error {
    anyhow!("XML error near byte {pos}: {e}")
}

pub fn attr(e: &BytesStart, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == key {
            return Some(match a.unescape_value() {
                Ok(v) => v.into_owned(),
                Err(_) => String::from_utf8_lossy(&a.value).into_owned(),
            });
        }
    }
    None
}

fn text_of(t: &BytesText) -> String {
    match t.unescape() {
        Ok(s) => s.into_owned(),
        Err(_) => String::from_utf8_lossy(t).into_owned(),
    }
}

fn cdata_of(c: &BytesCData) -> String {
    String::from_utf8_lossy(c).into_owned()
}

fn is_blank_icon(e: &BytesStart) -> bool {
    e.name().as_ref() == b"icon" && attr(e, b"src").map_or(true, |s| s.trim().is_empty())
}

/// Inside a <channel> we add a logo to, any existing <icon> goes (the logo we add
/// is a custom one that replaces it, or the channel had only blank ones).
/// Elsewhere only blank icons are replaced.
fn drops_icon(e: &BytesStart, channel_gets_logo: bool, replacing_icon: bool) -> bool {
    (channel_gets_logo && e.name().as_ref() == b"icon") || (replacing_icon && is_blank_icon(e))
}

fn has_src(e: &BytesStart) -> bool {
    attr(e, b"src").map_or(false, |s| !s.trim().is_empty())
}

fn open_top(e: &BytesStart) -> Top {
    match e.name().as_ref() {
        b"channel" => Top::Channel(ChannelInfo {
            id: attr(e, b"id").unwrap_or_default(),
            ..Default::default()
        }),
        b"programme" => Top::Programme(ProgInfo {
            channel: attr(e, b"channel").unwrap_or_default(),
            start: attr(e, b"start").unwrap_or_default(),
            stop: attr(e, b"stop").unwrap_or_default(),
            ..Default::default()
        }),
        _ => Top::Other,
    }
}

fn close_top(t: Top, out: &mut Scan) {
    match t {
        Top::Channel(c) => out.channels.push(c),
        Top::Programme(p) => out.programmes.push(p),
        Top::Other => {}
    }
}

fn note_child(top: &mut Top, e: &BytesStart) {
    let name = e.name();
    match top {
        Top::Channel(c) => {
            if name.as_ref() == b"icon" && has_src(e) {
                c.has_icon = true;
                if c.icon.is_none() {
                    c.icon = attr(e, b"src").map(|s| s.trim().to_string());
                }
            }
        }
        Top::Programme(p) => match name.as_ref() {
            b"icon" => {
                if has_src(e) {
                    p.has_icon = true;
                    if p.icon.is_none() {
                        p.icon = attr(e, b"src").map(|s| s.trim().to_string());
                    }
                }
            }
            b"rating" => p.has_rating = true,
            b"star-rating" => p.has_star = true,
            b"episode-num" => p.is_tv = true,
            _ => {}
        },
        Top::Other => {}
    }
}

fn finish_child(top: &mut Top, name: &[u8], text: &str) {
    let t = text.trim();
    if t.is_empty() {
        return;
    }
    match top {
        Top::Channel(c) => {
            if name == b"display-name" {
                c.names.push(t.to_string());
            }
        }
        Top::Programme(p) => match name {
            b"title" => {
                if p.title.is_none() {
                    p.title = Some(t.to_string());
                }
            }
            b"date" => p.date = Some(t.to_string()),
            b"category" => p.categories.push(t.to_string()),
            b"desc" => {
                p.has_desc = true;
                if p.desc.is_none() {
                    p.desc = Some(t.to_string());
                }
            }
            b"sub-title" => {
                if p.sub_title.is_none() {
                    p.sub_title = Some(t.to_string());
                }
            }
            b"episode-num" => {
                if p.episode.is_none() {
                    p.episode = Some(episode_label(t));
                }
            }
            b"rating" => {
                if p.rating.is_none() {
                    p.rating = Some(t.to_string());
                }
            }
            _ => {}
        },
        Top::Other => {}
    }
}

/// "0.4.0/1" (xmltv_ns, zero-based) becomes "S01E05"; anything else is kept as is.
pub fn episode_label(s: &str) -> String {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() == 3 {
        let num = |p: &str| p.split('/').next().unwrap_or("").trim().parse::<u32>().ok();
        match (num(parts[0]), num(parts[1])) {
            (Some(se), Some(ep)) => return format!("S{:02}E{:02}", se + 1, ep + 1),
            (None, Some(ep)) => return format!("E{:02}", ep + 1),
            _ => {}
        }
    }
    s.to_string()
}

/// First pass: what does the guide already contain?
pub fn scan(xml: &[u8]) -> Result<Scan> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Scan::default();
    // depth = number of open elements; <tv> is depth 1 once open.
    let mut depth = 0usize;
    let mut top = Top::Other;
    let mut child: Option<Vec<u8>> = None;
    let mut text = String::new();
    loop {
        let event = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(e) => return Err(xml_err(e, reader.buffer_position() as u64)),
        };
        match event {
            Event::Start(e) => {
                if depth == 1 {
                    top = open_top(&e);
                } else if depth == 2 {
                    note_child(&mut top, &e);
                    child = Some(e.name().as_ref().to_vec());
                    text.clear();
                }
                depth += 1;
            }
            Event::Empty(e) => {
                if depth == 1 {
                    close_top(open_top(&e), &mut out);
                } else if depth == 2 {
                    note_child(&mut top, &e);
                }
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 2 {
                    if let Some(name) = child.take() {
                        finish_child(&mut top, &name, &text);
                    }
                } else if depth == 1 {
                    close_top(std::mem::replace(&mut top, Top::Other), &mut out);
                }
            }
            Event::Text(t) => {
                // <rating><value>PG</value></rating>: the value is one level deeper.
                if child.is_some() && (depth == 3 || (depth == 4 && child.as_deref() == Some(&b"rating"[..]))) {
                    text.push_str(&text_of(&t));
                }
            }
            Event::CData(c) => {
                if depth == 3 && child.is_some() {
                    text.push_str(&cdata_of(&c));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn write_icon<T: Write>(w: &mut Writer<T>, src: &str) -> Result<()> {
    let mut el = BytesStart::new("icon");
    el.push_attribute(("src", src));
    w.write_event(Event::Empty(el))?;
    Ok(())
}

fn write_text_el<T: Write>(w: &mut Writer<T>, name: &str, text: &str) -> Result<()> {
    w.write_event(Event::Start(BytesStart::new(name)))?;
    w.write_event(Event::Text(BytesText::new(text)))?;
    w.write_event(Event::End(BytesEnd::new(name)))?;
    Ok(())
}

fn write_prog_additions<T: Write>(w: &mut Writer<T>, p: &ProgPlan) -> Result<()> {
    if let Some(d) = &p.desc {
        write_text_el(w, "desc", d)?;
    }
    if let Some(c) = &p.category {
        write_text_el(w, "category", c)?;
    }
    if let Some(i) = &p.icon {
        write_icon(w, i)?;
    }
    if let Some((system, value)) = &p.rating {
        let mut el = BytesStart::new("rating");
        el.push_attribute(("system", system.as_str()));
        w.write_event(Event::Start(el))?;
        write_text_el(w, "value", value)?;
        w.write_event(Event::End(BytesEnd::new("rating")))?;
    }
    if let Some((system, value)) = &p.star {
        let mut el = BytesStart::new("star-rating");
        el.push_attribute(("system", system.as_str()));
        w.write_event(Event::Start(el))?;
        write_text_el(w, "value", value)?;
        w.write_event(Event::End(BytesEnd::new("star-rating")))?;
    }
    Ok(())
}

/// Second pass: copy everything through and add what was planned.
/// `channel_logo` maps channel id to a logo URL for channels without one.
#[cfg_attr(not(test), allow(dead_code))]
pub fn rewrite(xml: &[u8], channel_logo: &HashMap<String, String>, plans: &[ProgPlan]) -> Result<Vec<u8>> {
    rewrite_into(xml, channel_logo, plans, Vec::with_capacity(xml.len() + xml.len() / 4))
}

/// Like `rewrite`, but streams into `out` (a file), so a large guide is never held in memory twice.
pub fn rewrite_into<T: Write>(xml: &[u8], channel_logo: &HashMap<String, String>, plans: &[ProgPlan], out: T) -> Result<T> {
    let mut reader = Reader::from_reader(xml);
    let mut writer = Writer::new(out);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut pidx = 0usize;
    // While Some(d), events are dropped until the depth returns to d.
    let mut skip_to: Option<usize> = None;
    let mut pending_plan: Option<&ProgPlan> = None;
    let mut pending_logo: Option<&String> = None;

    loop {
        let event = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(e) => return Err(xml_err(e, reader.buffer_position() as u64)),
        };

        if let Some(target) = skip_to {
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    depth = depth.saturating_sub(1);
                    if depth == target {
                        skip_to = None;
                    }
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
            continue;
        }

        // An empty <icon src=""/> is replaced when we have a real one to add.
        let replacing_icon = pending_logo.is_some() || pending_plan.map_or(false, |p| p.icon.is_some());

        match event {
            Event::Start(e) => {
                let mut drop_it = false;
                if depth == 1 {
                    match e.name().as_ref() {
                        b"programme" => {
                            let plan = plans.get(pidx);
                            pidx += 1;
                            if plan.map_or(false, |p| p.drop) {
                                drop_it = true;
                            } else {
                                pending_plan = plan;
                            }
                        }
                        b"channel" => {
                            pending_logo = attr(&e, b"id").and_then(|id| channel_logo.get(&id));
                        }
                        _ => {}
                    }
                } else if depth == 2 && drops_icon(&e, pending_logo.is_some(), replacing_icon) {
                    drop_it = true;
                }
                if drop_it {
                    skip_to = Some(depth);
                } else {
                    writer.write_event(Event::Start(e))?;
                }
                depth += 1;
            }
            Event::Empty(e) => {
                if depth == 1 {
                    let name = e.name().as_ref().to_vec();
                    if name == b"programme" {
                        let plan = plans.get(pidx);
                        pidx += 1;
                        match plan {
                            Some(p) if p.drop => {}
                            Some(p) if p.has_additions() => {
                                let end = e.to_end().into_owned();
                                writer.write_event(Event::Start(e))?;
                                write_prog_additions(&mut writer, p)?;
                                writer.write_event(Event::End(end))?;
                            }
                            _ => writer.write_event(Event::Empty(e))?,
                        }
                    } else if name == b"channel" {
                        let logo = attr(&e, b"id").and_then(|id| channel_logo.get(&id));
                        if let Some(url) = logo {
                            let end = e.to_end().into_owned();
                            writer.write_event(Event::Start(e))?;
                            write_icon(&mut writer, url)?;
                            writer.write_event(Event::End(end))?;
                        } else {
                            writer.write_event(Event::Empty(e))?;
                        }
                    } else {
                        writer.write_event(Event::Empty(e))?;
                    }
                } else if depth == 2 && drops_icon(&e, pending_logo.is_some(), replacing_icon) {
                    // dropped
                } else {
                    writer.write_event(Event::Empty(e))?;
                }
            }
            Event::End(e) => {
                depth = depth.saturating_sub(1);
                if depth == 1 {
                    if let Some(p) = pending_plan.take() {
                        write_prog_additions(&mut writer, p)?;
                    }
                    if let Some(url) = pending_logo.take() {
                        write_icon(&mut writer, url)?;
                    }
                }
                writer.write_event(Event::End(e))?;
            }
            Event::Eof => break,
            other => writer.write_event(other)?,
        }
        buf.clear();
    }
    Ok(writer.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = include_bytes!("../tests/sample.xml");

    #[test]
    fn scan_detects_logos_and_programme_details() {
        let s = scan(SAMPLE).unwrap();
        assert_eq!(s.channels.len(), 3);
        let icons: Vec<bool> = s.channels.iter().map(|c| c.has_icon).collect();
        assert_eq!(icons, vec![true, false, false], "empty src must not count as a logo");
        assert_eq!(s.channels[1].names, vec!["Empty Icon HD".to_string()]);

        assert_eq!(s.programmes.len(), 4);
        let p0 = &s.programmes[0];
        assert_eq!(p0.title.as_deref(), Some("The Sample Movie"));
        assert_eq!(p0.date.as_deref(), Some("1999"));
        assert_eq!(p0.categories, vec!["Film".to_string()]);
        assert!(p0.has_desc && !p0.is_tv && !p0.has_icon);
        assert!(s.programmes[1].is_tv);
        assert!(s.programmes[2].has_icon && s.programmes[2].has_rating);
        assert_eq!(s.programmes[3].channel, "NoLogo.ca");
        assert!(s.channels[0].icon.is_some() && s.channels[1].icon.is_none());
        assert_eq!(s.programmes[2].icon.as_deref(), Some("https://example.com/news.png"));
        assert_eq!(s.programmes[2].rating.as_deref(), Some("TV-G"));
        assert_eq!(s.programmes[1].episode.as_deref(), Some("S02E05"));
        assert_eq!(p0.desc.as_deref(), Some("A film about samples & tests."));
        assert_eq!(p0.stop, "20261126200000 +0000");
    }

    #[test]
    fn episode_labels() {
        assert_eq!(episode_label("0.4.0/1"), "S01E05");
        assert_eq!(episode_label(".11."), "E12");
        assert_eq!(episode_label("S3 E7"), "S3 E7");
    }

    #[test]
    fn rewrite_adds_only_missing_things_and_keeps_the_rest() {
        let mut logos = HashMap::new();
        logos.insert("EmptyIcon.uk".to_string(), "https://x/e.png".to_string());
        logos.insert("NoLogo.ca".to_string(), "https://x/n.png".to_string());
        let plans = vec![
            ProgPlan { category: Some("Movie".into()), ..Default::default() },
            ProgPlan {
                icon: Some("https://image.tmdb.org/t/p/w500/p.jpg".into()),
                rating: Some(("VCHIP".into(), "TV-PG".into())),
                desc: Some("Fish & chips <3".into()),
                ..Default::default()
            },
            ProgPlan::default(),
            ProgPlan { drop: true, ..Default::default() },
        ];
        let out = rewrite(SAMPLE, &logos, &plans).unwrap();
        let text = String::from_utf8(out.clone()).unwrap();

        // Original content survives byte for byte where untouched.
        assert!(text.contains("<!DOCTYPE tv SYSTEM \"xmltv.dtd\">"));
        assert!(text.contains("generator-info-name=\"sample\""));
        assert!(text.contains("samples &amp; tests"));
        assert!(text.contains("<icon src=\"https://example.com/haslogo.png\" width=\"100\" height=\"100\"/>"));
        // Our text is escaped, the empty icon is replaced, the dropped programme is gone.
        assert!(text.contains("Fish &amp; chips &lt;3"));
        assert!(!text.contains("src=\"\""));
        assert!(!text.contains("Premier League"));

        let s = scan(&out).unwrap();
        assert!(s.channels.iter().all(|c| c.has_icon));
        assert_eq!(s.programmes.len(), 3);
        assert_eq!(s.programmes[0].categories, vec!["Film".to_string(), "Movie".to_string()]);
        let p1 = &s.programmes[1];
        assert!(p1.has_icon && p1.has_rating && p1.has_desc);
        assert!(text.contains("<rating system=\"VCHIP\"><value>TV-PG</value></rating>"));
    }

    #[test]
    fn rewrite_without_plans_is_identity() {
        let out = rewrite(SAMPLE, &HashMap::new(), &[]).unwrap();
        assert_eq!(out, SAMPLE);
    }

    #[test]
    fn a_custom_logo_replaces_the_channel_icon() {
        let xml = br#"<tv><channel id="A"><display-name>A</display-name><icon src="http://old/a.png"/></channel></tv>"#;
        let mut logos = HashMap::new();
        logos.insert("A".to_string(), "http://new/a.png".to_string());
        let text = String::from_utf8(rewrite(xml, &logos, &[]).unwrap()).unwrap();
        assert!(!text.contains("old/a.png"), "{text}");
        assert_eq!(text.matches("<icon").count(), 1, "{text}");
        assert!(text.contains("new/a.png"), "{text}");
    }
}
