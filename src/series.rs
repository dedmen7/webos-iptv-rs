use crate::catalog::Item;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct SeriesProgress {
    pub series: Item,
    pub episode_id: String,
    pub season: u32,
    pub episode: u32,
    pub finished: bool,
}

impl SeriesProgress {
    pub fn matched_episode<'a>(&self, episodes: &'a [Item]) -> Option<&'a Item> {
        self.matched_index(episodes)
            .and_then(|index| episodes.get(index))
    }

    fn matched_index(&self, episodes: &[Item]) -> Option<usize> {
        let saved_number = (self.season, self.episode);
        episodes
            .iter()
            .position(|item| item.id == self.episode_id)
            .or_else(|| {
                if self.episode == 0 {
                    return None;
                }
                let mut matches = episodes
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| (item.season, item.episode) == saved_number);
                let (index, _) = matches.next()?;
                matches.next().is_none().then_some(index)
            })
    }

    pub fn target<'a>(&self, episodes: &'a [Item]) -> Option<&'a Item> {
        let saved_number = (self.season, self.episode);
        match self.matched_index(episodes) {
            Some(index) => episodes.get(index + usize::from(self.finished)),
            None if self.finished && self.episode != 0 => episodes
                .iter()
                .find(|item| item.episode != 0 && (item.season, item.episode) > saved_number),
            None => None,
        }
    }
}
