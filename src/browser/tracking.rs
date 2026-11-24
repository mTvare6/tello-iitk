use crate::{
    model::{Action, Location, Page},
    navigation::Navigation,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
};

pub use crate::tracking_model::{SavedView, action_key, location_key};

fn storage() -> Result<web_sys::Storage> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .context("Browser storage unavailable")
}

#[derive(Default, Serialize, Deserialize)]
pub struct Preferences {
    pub username: String,
    pub save_password: bool,
}

impl Preferences {
    pub fn load() -> Result<Self> {
        let username = storage()
            .ok()
            .and_then(|s| s.get_item("hello-iitk.username").ok().flatten())
            .unwrap_or_default();

        Ok(Self {
            username,
            save_password: false,
        })
    }

    pub fn save(&self) -> Result<()> {
        storage()?
            .set_item("hello-iitk.username", &self.username)
            .map_err(|_| anyhow::anyhow!("Could not save username"))
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    known: HashMap<String, Vec<String>>,
    order: Vec<String>,
    unread: HashSet<String>,
    resume: Vec<SavedView>,
}

pub struct Tracker {
    key: String,
    data: RefCell<Stored>,
    badges: RefCell<HashSet<String>>,
    pub resume: Vec<SavedView>,
    wake: Box<dyn Fn(Result<()>)>,
    last_resume: String,
}

impl Tracker {
    pub fn open(uid: &str, wake: impl Fn(Result<()>) + Send + 'static) -> Result<Self> {
        ensure!(
            !uid.is_empty() && uid.len() <= 32 && uid.bytes().all(|c| c.is_ascii_digit()),
            "Invalid account identity"
        );
        let key = format!("hello-iitk.state.{uid}");
        let data: Stored = match storage()?
            .get_item(&key)
            .map_err(|_| anyhow::anyhow!("Could not read navigation history"))?
        {
            Some(json) => serde_json::from_str(&json).context("Invalid saved navigation")?,
            None => Stored::default(),
        };
        ensure!(
            data.resume.len() <= 32 && data.known.len() <= 512,
            "Saved navigation is too large"
        );
        let resume = data.resume.clone();
        let last_resume = serde_json::to_string(&resume)?;
        let tracker = Self {
            key,
            data: RefCell::new(data),
            badges: RefCell::new(HashSet::new()),
            resume,
            wake: Box::new(wake),
            last_resume,
        };
        tracker.update_badges();

        Ok(tracker)
    }

    fn update_badges(&self) {
        let data = self.data.borrow();
        let mut badges = data.unread.clone();
        loop {
            let mut changed = false;
            for (page, entries) in &data.known {
                if !badges.contains(page) && entries.iter().any(|e| badges.contains(e)) {
                    badges.insert(page.clone());
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        *self.badges.borrow_mut() = badges;
    }

    fn save(&self) {
        let result = (|| -> Result<()> {
            let json = serde_json::to_string(&*self.data.borrow())?;
            storage()?.set_item(&self.key, &json).map_err(|_| {
                anyhow::anyhow!("Could not save navigation history; browser storage is full")
            })?;
            Ok(())
        })();
        (self.wake)(result);
    }

    pub fn observe(&self, location: &Location, page: &Page, opened: bool) {
        let key = location_key(location);
        let entries: Vec<String> = page
            .entries
            .iter()
            .filter_map(|e| action_key(&e.action))
            .take(20000)
            .collect();
        let mut data = self.data.borrow_mut();
        let inherited = data.unread.contains(&key);
        if let Some(known) = data.known.get(&key).cloned() {
            for entry in &entries {
                if !known.contains(entry) {
                    data.unread.insert(entry.clone());
                }
            }
        } else if inherited {
            data.unread.extend(entries.iter().cloned());
        }
        data.known.insert(key.clone(), entries);
        data.order.retain(|k| k != &key);
        data.order.push(key.clone());
        if opened {
            data.unread.remove(&key);
        }
        while data.known.len() > 512 || data.known.values().map(|v| v.len()).sum::<usize>() > 20000
        {
            if data.order.is_empty() {
                break;
            }
            let oldest = data.order.remove(0);
            data.known.remove(&oldest);
        }
        let existing: HashSet<String> = data.known.values().flatten().cloned().collect();
        data.unread.retain(|entry| existing.contains(entry));
        drop(data);
        self.update_badges();
        self.save();
    }

    pub fn is_new(&self, action: &Action) -> bool {
        action_key(action).is_some_and(|key| self.badges.borrow().contains(&key))
    }

    pub fn mark_read(&self, key: String) {
        self.data.borrow_mut().unread.remove(&key);
        self.update_badges();
        self.save();
    }

    pub fn home(&mut self) {
        self.data.borrow_mut().resume = vec![SavedView {
            location: Location::Catalog("/courses".into()),
            selected: 0,
            filter: String::new(),
            scroll: 0,
            text_focus: false,
        }];
        self.last_resume = serde_json::to_string(&self.data.borrow().resume).unwrap();
        self.save();
    }

    pub fn remember(&mut self, nav: &Navigation) {
        let views: Vec<SavedView> = nav
            .history
            .iter()
            .chain(nav.current.iter())
            .rev()
            .take(32)
            .map(|v| SavedView {
                location: v.location.clone(),
                selected: v.selected,
                filter: v.filter.clone(),
                scroll: v.scroll,
                text_focus: v.text_focus,
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if views.is_empty() {
            return;
        }
        let json = serde_json::to_string(&views).unwrap();
        if json != self.last_resume {
            self.last_resume = json;
            self.data.borrow_mut().resume = views;
            self.save();
        }
    }
}
