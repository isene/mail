//! Calendar invitations, as a reader wants them.
//!
//! A `text/calendar` part is a machine format: an hour-long invitation is
//! forty lines of `DTSTART;TZID=...`, folded at 75 octets, with the one
//! thing you want to know spread across three of them. This pulls out
//! what a person asks — what, when, where, who — and leaves the rest.
//!
//! [`Event::parse`] gives the fields; [`Event::to_text`] lays them out.
//! They are separate so a caller that colours its own output (the
//! desktop kastrup) and one that cannot (the phone) share the reading of
//! the file rather than each having its own.

/// One `VEVENT`, flattened to the fields worth showing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Event {
    /// `REQUEST`, `REPLY`, `CANCEL`, `PUBLISH` — see [`Event::kind`].
    pub method: String,
    pub summary: String,
    /// `DTSTART` as written, e.g. `20260814T093000`.
    pub start: String,
    pub end: String,
    pub timezone: String,
    pub location: String,
    pub organizer: String,
    /// Name (or address) and participation status, e.g. `("Alice", "accepted")`.
    pub attendees: Vec<(String, String)>,
    pub status: String,
    pub priority: String,
    /// The recurrence rule in words, e.g. `Weekly on Monday`.
    pub recurrence: String,
    pub description: String,
    pub all_day: bool,
}

impl Event {
    pub fn parse(ical: &str) -> Self {
        let mut e = Event::default();

        // Unfold continuation lines (RFC 5545): a long value is broken at
        // 75 octets and continued after a space or tab.
        let unfolded = ical
            .replace("\r\n ", "")
            .replace("\r\n\t", "")
            .replace("\n ", "")
            .replace("\n\t", "");

        for line in unfolded.lines() {
            let l = line.trim();
            if let Some(v) = l.strip_prefix("METHOD:") {
                e.method = v.to_string();
            } else if l.starts_with("SUMMARY;") {
                if let Some(pos) = l.find(':') { e.summary = unescape(&l[pos + 1..]); }
            } else if let Some(v) = l.strip_prefix("SUMMARY:") {
                e.summary = unescape(v);
            } else if l.starts_with("DTSTART") {
                if l.contains("VALUE=DATE:") { e.all_day = true; }
                if let Some(tz) = l.find("TZID=") {
                    e.timezone = l[tz + 5..].split(':').next().unwrap_or("").to_string();
                }
                if let Some(pos) = l.find(':') { e.start = l[pos + 1..].to_string(); }
            } else if l.starts_with("DTEND") {
                if let Some(pos) = l.find(':') { e.end = l[pos + 1..].to_string(); }
            } else if let Some(v) = l.strip_prefix("LOCATION:") {
                e.location = unescape(v).replace('\n', " ");
            } else if l.starts_with("ORGANIZER") {
                e.organizer = person(l).0;
            } else if l.starts_with("ATTENDEE") {
                let (name, _) = person(l);
                if name.is_empty() { continue; }
                let pstat = if l.contains("ACCEPTED") { "accepted" }
                    else if l.contains("DECLINED") { "declined" }
                    else if l.contains("TENTATIVE") { "tentative" }
                    else if l.contains("NEEDS-ACTION") { "needs action" }
                    else { "" };
                e.attendees.push((name, pstat.to_string()));
            } else if let Some(v) = l.strip_prefix("STATUS:") {
                e.status = v.to_string();
            } else if let Some(v) = l.strip_prefix("PRIORITY:") {
                e.priority = match v.trim() {
                    "1" | "2" => "High".into(),
                    "3" | "4" | "5" => "Normal".into(),
                    "6" | "7" | "8" | "9" => "Low".into(),
                    other => other.to_string(),
                };
            } else if let Some(v) = l.strip_prefix("RRULE:") {
                e.recurrence = recurrence(v);
            } else if let Some(v) = l.strip_prefix("DESCRIPTION:") {
                e.description = unescape(v);
            }
        }
        e
    }

    /// What this is: an invitation, a reply to one, a cancellation.
    pub fn kind(&self) -> &'static str {
        match self.method.to_uppercase().as_str() {
            "REPLY" => "Calendar Reply",
            "REQUEST" => "Calendar Invite",
            "CANCEL" => "Cancellation",
            _ => "Calendar Event",
        }
    }

    /// The times as one phrase: `2026-08-14 09:30 - 10:30 (Friday)`, or
    /// the day alone when the event owns the whole of it.
    pub fn when(&self) -> String {
        let s = fmt_dt(&self.start);
        let d = fmt_dt(&self.end);
        let day = weekday(&self.start);
        if self.all_day {
            if self.end.is_empty() || self.end == self.start {
                return format!("{} ({}) - All day", s, day);
            }
            return format!("{} to {} - All day", s, d);
        }
        if self.start.is_empty() { return String::new(); }
        if self.end.is_empty() { return format!("{} ({})", s, day); }
        // Within one day the date is said once, and the end is a time.
        if s.len() > 10 && d.len() > 10 && s[..10] == d[..10] {
            format!("{} - {} ({})", s, &d[11..], day)
        } else {
            format!("{} - {} ({})", s, d, day)
        }
    }

    /// The whole thing as plain text, for a reader with no colours.
    pub fn to_text(&self) -> String {
        let mut lines = vec![format!("[{}]", self.kind()), String::new()];
        let mut row = |label: &str, value: &str| {
            if !value.is_empty() { lines.push(format!("{} {}", label, value)); }
        };
        row("WHAT: ", &self.summary);
        row("WHEN: ", &self.when());
        row("  TZ: ", &self.timezone);
        row("WHERE:", &self.location);
        row("RECUR:", &self.recurrence);
        row("STATUS:", &self.status);
        row("PRIORITY:", &self.priority);
        if !self.organizer.is_empty() || !self.attendees.is_empty() {
            lines.push(String::new());
        }
        if !self.organizer.is_empty() {
            lines.push(format!("ORGANIZER: {}", self.organizer));
        }
        if !self.attendees.is_empty() {
            lines.push("PARTICIPANTS:".to_string());
            for (name, pstat) in &self.attendees {
                if pstat.is_empty() {
                    lines.push(format!("  {}", name));
                } else {
                    lines.push(format!("  {} ({})", name, pstat));
                }
            }
        }
        if !self.description.is_empty() {
            lines.push(String::new());
            lines.push("DESCRIPTION:".to_string());
            for d in self.description.lines() { lines.push(d.to_string()); }
        }
        lines.join("\n")
    }
}

/// Parse and lay out in one call — what a body-text pipeline wants.
pub fn summary(ical: &str) -> String {
    Event::parse(ical).to_text()
}

/// The `CN=` name and address off an `ORGANIZER` / `ATTENDEE` line.
fn person(l: &str) -> (String, String) {
    let email = l.to_lowercase().find("mailto:").map(|i| {
        l[i + 7..]
            .split(|c: char| c == ';' || c == '>' || c == '\n')
            .next()
            .unwrap_or("")
            .to_string()
    });
    if let Some(cn) = l.find("CN=") {
        let name = l[cn + 3..]
            .trim_start_matches('"')
            .split(|c: char| c == ';' || c == ':' || c == '"')
            .next()
            .unwrap_or("")
            .to_string();
        let shown = match &email {
            Some(e) if !e.is_empty() => format!("{} <{}>", name, e),
            _ => name,
        };
        return (shown, email.unwrap_or_default());
    }
    match email {
        Some(e) => (e.clone(), e),
        None => (String::new(), String::new()),
    }
}

/// `20260814T093000` → `2026-08-14 09:30`.
fn fmt_dt(s: &str) -> String {
    if s.len() < 8 { return s.to_string(); }
    if !s[..8].chars().all(|c| c.is_ascii_digit()) { return s.to_string(); }
    let date = format!("{}-{}-{}", &s[0..4], &s[4..6], &s[6..8]);
    match s.find('T') {
        Some(t) if s.len() >= t + 5 => {
            let time = &s[t + 1..];
            format!("{} {}:{}", date, &time[0..2], &time[2..4])
        }
        _ => date,
    }
}

/// The day of the week a `YYYYMMDD…` stamp falls on. Zeller's formula,
/// so no calendar dependency for one line of output.
fn weekday(s: &str) -> &'static str {
    if s.len() < 8 { return ""; }
    let (y, m, d) = match (
        s[0..4].parse::<i32>(),
        s[4..6].parse::<u32>(),
        s[6..8].parse::<u32>(),
    ) {
        (Ok(y), Ok(m), Ok(d)) => (y, m, d),
        _ => return "",
    };
    let (yy, mm) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    let k = yy % 100;
    let j = yy / 100;
    let h = (d as i32 + (13 * (mm as i32 + 1)) / 5 + k + k / 4 + j / 4 + 5 * j) % 7;
    match ((h + 5) % 7 + 1) as u32 {
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        6 => "Saturday",
        _ => "Sunday",
    }
}

/// An `RRULE` in words: `Every 2 weeks on Monday, Wednesday, 10 times`.
pub fn recurrence(rrule: &str) -> String {
    let mut parts = std::collections::HashMap::new();
    for p in rrule.split(';') {
        if let Some((k, v)) = p.split_once('=') { parts.insert(k, v); }
    }
    let interval = parts.get("INTERVAL").copied().unwrap_or("1");
    let every = |unit: &str, plural: &str| -> String {
        if interval == "1" { unit.to_string() } else { format!("Every {} {}", interval, plural) }
    };
    let mut s = match parts.get("FREQ").copied().unwrap_or("") {
        "DAILY" => every("Daily", "days"),
        "WEEKLY" => every("Weekly", "weeks"),
        "MONTHLY" => every("Monthly", "months"),
        "YEARLY" => every("Yearly", "years"),
        other => other.to_string(),
    };
    if let Some(days) = parts.get("BYDAY") {
        let names: Vec<&str> = days
            .split(',')
            .map(|d| match d.trim_start_matches(|c: char| c == '-' || c.is_ascii_digit()) {
                "MO" => "Monday",
                "TU" => "Tuesday",
                "WE" => "Wednesday",
                "TH" => "Thursday",
                "FR" => "Friday",
                "SA" => "Saturday",
                "SU" => "Sunday",
                o => o,
            })
            .collect();
        if !names.is_empty() { s.push_str(&format!(" on {}", names.join(", "))); }
    }
    if let Some(count) = parts.get("COUNT") {
        s.push_str(&format!(", {} times", count));
    } else if let Some(until) = parts.get("UNTIL") {
        s.push_str(&format!(", until {}", fmt_dt(until)));
    }
    s
}

/// RFC 5545 text escapes: `\n`, `\,`, `\;`, `\\`.
fn unescape(v: &str) -> String {
    v.replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVITE: &str = "BEGIN:VCALENDAR\r\n\
        METHOD:REQUEST\r\n\
        BEGIN:VEVENT\r\n\
        SUMMARY:Team Infrastructure: Koordinering\r\n\
        DTSTART;TZID=W. Europe Standard Time:20260814T093000\r\n\
        DTEND;TZID=W. Europe Standard Time:20260814T101500\r\n\
        LOCATION:Microsoft Teams Meeting\r\n\
        ORGANIZER;CN=\"Alice Example\":mailto:alice@example.com\r\n\
        ATTENDEE;CN=Bob;PARTSTAT=ACCEPTED:mailto:bob@example.com\r\n\
        ATTENDEE;CN=Carol;PARTSTAT=NEEDS-ACTION:mailto:carol@example.com\r\n\
        RRULE:FREQ=WEEKLY;INTERVAL=1;BYDAY=FR\r\n\
        STATUS:CONFIRMED\r\n\
        DESCRIPTION:Weekly sync\\, bring notes\r\n\
        END:VEVENT\r\n\
        END:VCALENDAR\r\n";

    #[test]
    fn an_invitation_reads_as_one() {
        let e = Event::parse(INVITE);
        assert_eq!(e.kind(), "Calendar Invite");
        assert_eq!(e.summary, "Team Infrastructure: Koordinering");
        assert_eq!(e.timezone, "W. Europe Standard Time");
        assert_eq!(e.organizer, "Alice Example <alice@example.com>");
        assert_eq!(e.attendees.len(), 2);
        assert_eq!(e.attendees[1], ("Carol <carol@example.com>".into(), "needs action".into()));
        assert_eq!(e.recurrence, "Weekly on Friday");
        // An escaped comma is a comma.
        assert_eq!(e.description, "Weekly sync, bring notes");
    }

    #[test]
    fn one_day_says_the_date_once() {
        let e = Event::parse(INVITE);
        assert_eq!(e.when(), "2026-08-14 09:30 - 10:15 (Friday)");
    }

    #[test]
    fn an_all_day_event_has_no_times() {
        let e = Event::parse(
            "BEGIN:VEVENT\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20261224\r\nEND:VEVENT\r\n",
        );
        assert_eq!(e.when(), "2026-12-24 (Thursday) - All day");
    }

    #[test]
    fn a_folded_line_is_read_whole() {
        // RFC 5545 breaks a long value and continues it after a space.
        let e = Event::parse("BEGIN:VEVENT\r\nSUMMARY:A rather long meeting ti\r\n tle\r\nEND:VEVENT\r\n");
        assert_eq!(e.summary, "A rather long meeting title");
    }

    #[test]
    fn the_text_leads_with_what_it_is() {
        let text = summary(INVITE);
        assert!(text.starts_with("[Calendar Invite]"), "{}", text);
        assert!(text.contains("WHAT:  Team Infrastructure"), "{}", text);
        assert!(text.contains("WHEN:  2026-08-14 09:30 - 10:15 (Friday)"), "{}", text);
        // And none of the machine format survives.
        assert!(!text.contains("DTSTART"), "{}", text);
    }
}
