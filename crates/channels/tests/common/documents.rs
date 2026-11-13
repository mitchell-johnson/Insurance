//! The documents module for the portal's tests: it holds a few documents
//! with their owners and answers a download by the documents module's own
//! rule: a party gets only what is theirs, staff never get a clinical one.

use std::sync::Mutex;

use ic_channels::documents::{Asker, Documents, Fetched};

type Asked<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Fetched, String>> + Send + 'a>>;

#[derive(Default)]
pub struct DocumentsDouble {
    pub asked: Mutex<Vec<(String, Asker)>>,
}

fn answer(status: u16) -> Fetched {
    Fetched {
        status,
        content_type: "application/problem+json".into(),
        body: vec![],
    }
}

impl Documents for DocumentsDouble {
    fn pdf(&self, id: &str, asker: Asker) -> Asked<'_> {
        self.asked.lock().unwrap().push((id.to_string(), asker.clone()));
        // doc-1: Kiri's schedule. doc-clinical: a claim's medical report. doc-gone: destroyed. doc-busy: the module is down.
        let got = match (id, &asker) {
            ("doc-busy", _) => Err("connection refused".to_string()),
            ("doc-gone", _) => Ok(answer(410)),
            ("doc-odd", _) => Ok(answer(500)),
            ("doc-1", Asker::Party(p)) if p == "party-00000001" => Ok(Fetched {
                status: 200,
                content_type: "application/pdf".into(),
                body: b"%PDF-1.7 schedule".to_vec(),
            }),
            ("doc-1", Asker::Staff { .. }) => Ok(Fetched {
                status: 200,
                content_type: "application/pdf".into(),
                body: b"%PDF-1.7 schedule".to_vec(),
            }),
            ("doc-clinical", Asker::Staff { .. }) => Ok(answer(403)),
            _ => Ok(answer(404)),
        };
        Box::pin(async move { got })
    }
}
