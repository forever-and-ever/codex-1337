//! Musters a prepared catalog for amendments: every model-visible entry, unbudgeted and
//! unaliased, so an amendment line stays valid whatever alias table opened the listing.

use super::PreparedSkillCatalog;
use crate::roster::Enlistment;
use crate::roster::Muster;

impl PreparedSkillCatalog<'_> {
    pub(crate) fn muster(&self) -> Muster {
        self.unaliased()
            .skills
            .iter()
            .map(|line| Enlistment::new(line.render_full(), line.render_minimum()))
            .collect()
    }
}
