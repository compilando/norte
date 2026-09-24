//! The connections picker (#140): what connections are configured and which
//! one is being chosen.
//!
//! Lives here and not in a frontend because of rule 7, and because the GUI
//! will need the same picker with a different painter. What this module does
//! NOT do is read the file: the rows are handed to it already read, same as
//! the layout picker — whoever has the disk in front of them is the
//! frontend.

/// A configured connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The name it has in `connections.toml`.
    pub name: String,
    /// Its URL. **Never a secret**: a `ConnectionSpec` references its
    /// credentials (ADR 0015) and only the address travels here.
    pub url: String,
    /// What is wrong with this entry, if anything is (#365).
    ///
    /// `Some` = norte could not read it, and then the row **cannot be
    /// chosen**: there is nowhere to go. It still shows up on purpose,
    /// because the reader wrote that connection and expected to see it —
    /// making it disappear would leave them looking for why it is missing,
    /// which is exactly what used to happen when one bad entry took down the
    /// whole list.
    pub issue: Option<String>,
}

impl Row {
    /// A row that does lead somewhere.
    #[must_use]
    pub fn buena(name: String, url: String) -> Self {
        Self {
            name,
            url,
            issue: None,
        }
    }

    /// One that does not, with its reason. The URL is empty: there is none to
    /// give, and inventing text for the gap would be painting something
    /// nobody wrote.
    #[must_use]
    pub fn inservible(name: String, reason: String) -> Self {
        Self {
            name,
            url: String::new(),
            issue: Some(reason),
        }
    }

    /// Can it be gone to?
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        self.issue.is_none()
    }
}

/// The connections picker.
#[derive(Debug)]
pub struct ConnectionsPicker {
    rows: Vec<Row>,
    cursor: usize,
}

impl ConnectionsPicker {
    /// Opens the picker with the connections it is given.
    ///
    /// An EMPTY list is legitimate —not having any connections configured is
    /// the normal state on day one— and it can still be opened: the frontend
    /// paints that there are none and where to add them, which is more
    /// useful than a dead key.
    #[must_use]
    pub fn open(rows: Vec<Row>) -> Self {
        Self { rows, cursor: 0 }
    }

    /// The rows, in the order they arrived.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Where the cursor is, clamped to the rows there are.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor.min(self.rows.len().saturating_sub(1))
    }

    /// Moves up.
    pub const fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves down.
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    /// The URL of the highlighted row, if there is one AND it can be gone to.
    ///
    /// `None` over an unusable row (#365), and that is what stops a dead
    /// button: the row is visible, it says what is wrong with it, and
    /// confirming it does not navigate anywhere — which is the honest thing,
    /// because there is nowhere to go.
    #[must_use]
    pub fn chosen(&self) -> Option<&str> {
        self.rows
            .get(self.cursor())
            .filter(|r| r.is_selectable())
            .map(|r| r.url.as_str())
    }

    /// The reason for the highlighted row, if it is one of the ones that do
    /// not work.
    ///
    /// The frontend paints it when trying to choose it: showing the reason
    /// THERE, at the moment the reader tries, is what turns "this does
    /// nothing" into "this does not work, and here is why".
    #[must_use]
    pub fn issue(&self) -> Option<&str> {
        self.rows
            .get(self.cursor())
            .and_then(|r| r.issue.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Row> {
        vec![
            Row::buena("home".into(), "sftp://home/".into()),
            Row::buena("bucket".into(), "s3://bucket/".into()),
        ]
    }

    /// **An entry norte cannot read IS SHOWN and cannot be chosen** (#365).
    ///
    /// Both halves matter and neither alone is enough. That it shows,
    /// because the reader wrote it and making it disappear leaves them
    /// looking for why it is missing — which is what used to happen when one
    /// bad entry took down the whole list. And that it cannot be chosen,
    /// because there is nowhere to go: offering it would be a dead button.
    #[test]
    fn an_unusable_connection_shows_but_leads_nowhere() {
        let mut p = ConnectionsPicker::open(vec![
            Row::inservible("broken".into(), "unknown field `password`".into()),
            Row::buena("home".into(), "sftp://home/".into()),
        ]);
        assert_eq!(p.rows().len(), 2, "the broken one stays in the list");
        assert_eq!(p.chosen(), None, "and it cannot be gone to");
        assert_eq!(
            p.issue(),
            Some("unknown field `password`"),
            "and it says what is wrong, which is the actionable part"
        );
        // The good one next to it is still selectable: that is the fix.
        p.down();
        assert_eq!(p.chosen(), Some("sftp://home/"));
        assert_eq!(p.issue(), None);
    }

    /// The cursor moves and stays INSIDE at both ends: a picker that runs off
    /// the top chooses something that is not being looked at.
    #[test]
    fn the_cursor_does_not_run_off_either_end() {
        let mut p = ConnectionsPicker::open(rows());
        p.up();
        assert_eq!(p.cursor(), 0);
        p.down();
        p.down();
        p.down();
        assert_eq!(p.cursor(), 1);
        assert_eq!(p.chosen(), Some("s3://bucket/"));
    }

    /// With no connections configured the picker still opens and chooses
    /// nothing: showing "you have none" is more useful than a key that does
    /// nothing.
    #[test]
    fn with_no_connections_it_opens_and_chooses_nothing() {
        let mut p = ConnectionsPicker::open(Vec::new());
        assert!(p.rows().is_empty());
        assert_eq!(p.chosen(), None);
        p.down();
        assert_eq!(p.cursor(), 0);
    }
}
