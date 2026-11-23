use super::*;

/// A platform that reads `lid` and `idle`.
struct Reading {
    lid: Option<Lid>,
    idle: bool,
}

impl Platform for Reading {
    async fn lid(&mut self) -> Result<Option<Lid>> {
        Ok(self.lid)
    }

    async fn idle(&mut self) -> Result<bool> {
        Ok(self.idle)
    }
}

async fn reads_away(lid: Option<(bool, bool)>, idle: bool) -> bool {
    let lid = lid.map(|(closed, docked)| Lid { closed, docked });
    away(&mut Reading { lid, idle }).await.unwrap()
}

#[tokio::test]
async fn a_closed_lid_is_away_unless_docked_where_idleness_decides() {
    // An open lid is the user there, idle or not.
    assert!(!reads_away(Some((false, false)), true).await);
    assert!(reads_away(Some((true, false)), false).await);
    // A docked laptop with its lid closed is in use while it gets input.
    assert!(!reads_away(Some((true, true)), false).await);
    assert!(reads_away(Some((true, true)), true).await);
    // Without a lid, idleness decides.
    assert!(!reads_away(None, false).await);
    assert!(reads_away(None, true).await);
}
