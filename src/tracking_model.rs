use crate::{
    auth,
    model::{Action, Location, Page},
    navigation::View,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedView {
    pub location: Location,
    pub selected: usize,
    pub filter: String,
    pub scroll: u16,
    pub text_focus: bool,
}

impl SavedView {
    pub fn apply(&self, page: Page) -> View {
        let mut view = View::new(self.location.clone(), page);
        view.set_filter(self.filter.clone());
        view.selected = self.selected.min(view.visible().len().saturating_sub(1));
        view.scroll = self.scroll;
        view.text_focus = self.text_focus && !view.page.text.is_empty();

        view
    }
}

pub fn location_key(location: &Location) -> String {
    let parts: Vec<String> = match location {
        Location::Catalog(path) => vec!["catalog".into(), path.clone()],
        Location::Course(c) => vec!["course".into(), c.id.clone()],
        Location::Collection(c, s) => vec!["section".into(), c.id.clone(), s.endpoint().into()],
        Location::Week(c, w) => vec!["week".into(), c.id.clone(), w.clone()],
        Location::Item(c, s, id) => {
            vec!["item".into(), c.id.clone(), s.endpoint().into(), id.clone()]
        }
    };

    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&parts).expect("String identities serialize"))
    )
}

pub fn action_key(action: &Action) -> Option<String> {
    match action {
        Action::Navigate(l) => Some(location_key(l)),
        Action::File(file) => {
            let mut url = url::Url::parse(auth::ORIGIN).ok()?.join(&file.url).ok()?;
            url.set_query(None);
            url.set_fragment(None);

            Some(format!(
                "{:x}",
                Sha256::digest(format!("file:{}:{url}", file.id).as_bytes())
            ))
        }
        _ => None,
    }
}
