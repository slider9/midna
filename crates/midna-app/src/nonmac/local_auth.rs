//! `local_auth` off macOS: the owner check (Touch ID or the account password) that letting
//! another Mac drive this one needs. Nearby isn't on Windows yet (midnad refuses it), so there's
//! nothing to check for: every ask fails at once, and nothing is allowed.
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)] // Allowed and Cancelled come only from macOS's prompt
pub enum Outcome {
    Allowed,
    Cancelled,
    Failed(String),
}

pub struct Asking {
    pub answer: async_channel::Receiver<Outcome>,
}

impl Asking {
    #[allow(dead_code)]
    pub fn cancel(&self) {}
}

pub fn ask(_reason: &str) -> Asking {
    let (tx, answer) = async_channel::bounded(1);
    let _ = tx.try_send(Outcome::Failed("Nearby isn't on Windows yet".into()));
    Asking { answer }
}
