use crate::auth::ORIGIN;
use anyhow::{Context, Result, bail};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Course {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Section {
    Handout,
    Lectures,
    Announcements,
    Resources,
    Assignments,
    Quizzes,
    Exams,
}

impl Section {
    pub const ALL: [Self; 7] = [
        Self::Handout,
        Self::Lectures,
        Self::Announcements,
        Self::Resources,
        Self::Assignments,
        Self::Quizzes,
        Self::Exams,
    ];

    pub fn endpoint(self) -> &'static str {
        match self {
            Self::Handout => "first_course_handouts",
            Self::Lectures => "lectures",
            Self::Announcements => "announcements/type/all",
            Self::Resources => "resources",
            Self::Assignments => "assignments",
            Self::Quizzes => "quizzes",
            Self::Exams => "exams",
        }
    }

    pub fn permission(self) -> &'static str {
        match self {
            Self::Announcements => "announcements",
            _ => self.endpoint(),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Handout => "Course handout",
            Self::Lectures => "Lectures",
            Self::Announcements => "Announcements",
            Self::Resources => "Resources",
            Self::Assignments => "Assignments",
            Self::Quizzes => "Quizzes",
            Self::Exams => "Exams",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Location {
    Catalog(String),
    Course(Course),
    Collection(Course, Section),
    Week(Course, String),
    Item(Course, Section, String),
}

impl Location {
    pub fn label(&self) -> String {
        match self {
            Self::Catalog(_) => "My courses".into(),
            Self::Course(c) => c.title.clone(),
            Self::Collection(_, s) => s.label().into(),
            Self::Week(_, w) => w.clone(),
            Self::Item(_, _, _) => "Item".into(),
        }
    }

    pub fn course(&self) -> Option<&Course> {
        match self {
            Self::Catalog(_) => None,
            Self::Course(c) | Self::Collection(c, _) | Self::Week(c, _) | Self::Item(c, _, _) => {
                Some(c)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Navigate(Location),
    File(Attachment),
    External(Url),
    None,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub title: String,
    pub subtitle: String,
    pub action: Action,
}

#[derive(Clone, Debug)]
pub struct Page {
    pub title: String,
    pub text: String,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Attachment {
    pub id: i64,
    pub filename: String,
    #[serde(rename = "fileUrl")]
    pub url: String,
    #[serde(default)]
    pub filesize: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Item {
    // Handouts omit the id.
    #[serde(default)]
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub instructions: Option<String>,
    #[serde(default)]
    pub files: Vec<Attachment>,
    #[serde(default, rename = "solFiles")]
    pub solution_files: Vec<Attachment>,
    pub week: Option<String>,
    pub topic: Option<String>,
    #[serde(rename = "videoUrl")]
    pub video_url: Option<String>,
    #[serde(flatten)]
    pub fields: serde_json::Map<String, Value>,
}

impl Item {
    pub fn key(&self) -> String {
        self.id.to_string()
    }

    pub fn week_label(&self) -> String {
        self.week
            .as_deref()
            .filter(|w| !w.is_empty())
            .unwrap_or("Ungrouped lectures")
            .to_string()
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();

        if let Some(t) = &self.topic
            && !t.is_empty()
        {
            parts.push(clean(t));
        }

        for (field, label) in [
            ("endDate", "Due"),
            ("releaseOn", "Released"),
            ("created", "Posted"),
        ] {
            if let Some(n) = self
                .fields
                .get(field)
                .and_then(Value::as_i64)
                .filter(|n| *n > 0)
            {
                parts.push(format!("{label}: {}", timestamp(n)));
                break;
            }
        }

        parts.push(format!("{} attachment(s)", self.files.len()));

        parts.join(" · ")
    }

    pub fn page(&self) -> Page {
        let mut text = String::new();

        for html in [self.description.as_deref(), self.instructions.as_deref()]
            .into_iter()
            .flatten()
        {
            text.push_str(html);
            text.push_str("<br><br>");
        }

        // Handout fields already contain HTML.
        for (field, label) in [
            ("syllabus", "Syllabus"),
            ("team", "Team"),
            ("venue", "Venue"),
            ("gradingPolicy", "Grading policy"),
            ("textReferences", "References"),
            ("outcomes", "Outcomes"),
            ("audience", "Audience"),
        ] {
            if let Some(s) = self
                .fields
                .get(field)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                text.push_str(&format!("<h3>{label}</h3>{s}"));
            }
        }

        let mut metadata = Vec::new();

        for (field, label) in [
            ("releaseOn", "Release"),
            ("startDate", "Starts"),
            ("endDate", "Deadline"),
            ("endDateDap", "Late deadline"),
            ("resultsDate", "Results"),
            ("created", "Posted"),
        ] {
            if let Some(n) = self
                .fields
                .get(field)
                .and_then(Value::as_i64)
                .filter(|n| *n > 0)
            {
                metadata.push(format!("{label}: {}", timestamp(n)));
            }
        }

        for (field, label) in [
            ("totalScore", "Total score"),
            ("submitted", "Submitted"),
            ("visited", "Visited"),
            ("resumeFrom", "Resume position (seconds)"),
        ] {
            if let Some(v) = self.fields.get(field) {
                metadata.push(format!("{label}: {v}"));
            }
        }

        if !metadata.is_empty() {
            text.push_str(&format!(
                "<pre>{}</pre>",
                metadata
                    .join("\n")
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
            ));
        }

        let mut entries = Vec::new();

        if let Some(s) = self.video_url.as_deref().filter(|s| !s.is_empty())
            && let Ok(url) = Url::parse(s)
            && matches!(url.scheme(), "https" | "http")
        {
            entries.push(Entry {
                title: "Play video in browser".into(),
                subtitle: url.host_str().unwrap_or("").into(),
                action: link_action(url, self.id),
            });
        }

        let doc = Html::parse_fragment(&text);

        let mut seen = std::collections::HashSet::new();

        for anchor in doc.select(&selector("a[href]")) {
            let Some(href) = anchor.value().attr("href") else {
                continue;
            };
            let Ok(url) = Url::parse(ORIGIN).unwrap().join(href) else {
                continue;
            };

            if !matches!(url.scheme(), "http" | "https") || !seen.insert(url.to_string()) {
                continue;
            }

            // Internal links can trigger actions on the site.
            // /manage files need the logged-in session.
            if url.host_str() == Some("hello.iitk.ac.in") && !url.path().starts_with("/manage/") {
                continue;
            }

            let label = node_text(anchor);
            entries.push(Entry {
                title: format!(
                    "Open link: {}",
                    if label.is_empty() {
                        url.host_str().unwrap_or("")
                    } else {
                        &label
                    }
                ),
                subtitle: url.host_str().unwrap_or("").into(),
                action: link_action(url, self.id),
            });
        }

        for (files, prefix) in [(&self.files, ""), (&self.solution_files, "Solution: ")] {
            for f in files {
                entries.push(Entry {
                    title: format!("{prefix}{}", clean(&f.filename)),
                    subtitle: format!("{} bytes · attachment", f.filesize),
                    action: Action::File(f.clone()),
                });
            }
        }

        if text.is_empty() {
            text = "<p>No text description. Select an attachment below.</p>".into();
        }

        Page {
            title: clean(&self.title),
            text,
            entries,
        }
    }
}

pub fn timestamp(seconds: i64) -> String {
    let local = time::OffsetDateTime::from_unix_timestamp(seconds)
        .ok()
        .and_then(|utc| utc.checked_to_offset(time::UtcOffset::from_hms(5, 30, 0).ok()?));

    match local {
        Some(date) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02} IST",
            date.year(),
            date.month() as u8,
            date.day(),
            date.hour(),
            date.minute()
        ),
        None => seconds.to_string(),
    }
}

/// Strip control codes before drawing text.
pub fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}

fn node_text(node: scraper::ElementRef<'_>) -> String {
    clean(
        &node
            .text()
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

pub fn parse_catalog(html: &str, current: &str) -> Result<Page> {
    let doc = Html::parse_document(html);

    if doc
        .select(&selector("form.user-login-form, body.anonymous"))
        .next()
        .is_some()
    {
        bail!("Session expired. Run `hello-iitk login`");
    }

    if doc.select(&selector(".courses-list")).next().is_none() {
        bail!("Course catalog markup changed: .courses-list not found");
    }

    let base = Url::parse(ORIGIN)?;

    let mut entries = Vec::new();

    for card in doc.select(&selector(".course-card")) {
        let Some(link) = card
            .select(&selector("a[href]"))
            .find_map(|a| a.value().attr("href"))
        else {
            continue;
        };

        let url = base.join(link)?;
        let segments: Vec<_> = url.path_segments().context("Invalid course URL")?.collect();

        if url.origin() != base.origin()
            || segments.len() != 3
            || segments[0] != "studio"
            || segments[2] != "student"
        {
            continue;
        }

        let id = segments[1].to_owned();
        validate_course_id(&id)?;

        let code = card
            .select(&selector(".course-id"))
            .next()
            .map(node_text)
            .unwrap_or_default();

        let name = card
            .select(&selector(".title"))
            .next()
            .map(node_text)
            .unwrap_or_else(|| id.clone());

        let title = if code.is_empty() {
            name
        } else {
            format!("{code} · {name}")
        };

        entries.push(Entry {
            title: title.clone(),
            subtitle: "Course".into(),
            action: Action::Navigate(Location::Course(Course { id, title })),
        });
    }

    let mut seen = std::collections::HashSet::new();

    for a in doc.select(&selector("a[href]")) {
        let Some(href) = a.value().attr("href") else {
            continue;
        };
        let Ok(url) = base.join(href) else { continue };

        if url.origin() != base.origin()
            || !(url.path() == "/courses" || url.path().starts_with("/courses/"))
        {
            continue;
        }

        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );

        if path == current || !seen.insert(path.clone()) {
            continue;
        }

        let title = node_text(a);

        if title.is_empty() {
            continue;
        }

        entries.push(Entry {
            title: format!("Browse: {title}"),
            subtitle: "Catalog / semester / page".into(),
            action: Action::Navigate(Location::Catalog(path)),
        });
    }

    Ok(Page {
        title: "My courses".into(),
        text: String::new(),
        entries,
    })
}

pub fn validate_course_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("Invalid course identifier");
    }

    Ok(())
}

pub fn parse_items(value: Value, handout: bool) -> Result<Vec<Item>> {
    let data = value
        .get("data")
        .context("API response missing data")?
        .clone();

    if handout {
        Ok(vec![
            serde_json::from_value(data).context("Invalid handout schema")?,
        ])
    } else {
        if !data.as_array().is_some_and(|items| {
            items
                .iter()
                .all(|i| i.get("id").and_then(Value::as_i64).is_some())
        }) {
            bail!("Invalid content list schema: expected array of items with integer ids");
        }

        serde_json::from_value(data).context("Invalid content list schema")
    }
}

fn link_action(url: Url, id: i64) -> Action {
    if url.host_str() == Some("hello.iitk.ac.in") {
        let filename = url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .filter(|s| !s.is_empty())
            .unwrap_or("attachment")
            .to_string();

        Action::File(Attachment {
            id,
            filename,
            url: url.to_string(),
            filesize: 0,
        })
    } else {
        Action::External(url)
    }
}
