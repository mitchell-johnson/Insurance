//! `ic-portal`: the portal service for the customer app, the adviser and
//! employer portals, the staff console and the operations console.
//!
//! A stateless gateway to the channel module on the ledger nodes.
//! Configuration is by environment variable; see
//! [`ic_channels::settings::PortalSettings::from_lookup`].

use std::sync::Arc;

use ic_channels::forward::{self, forward};
use ic_channels::http::{router, AppState, Hosted};
use ic_channels::ops::Live;
use ic_channels::serve::{heartbeat, wall_clock};
use ic_channels::settings::{Identity, PortalSettings};

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = term => {} }
    tracing::info!("shutting down");
}

#[tokio::main]
async fn main() -> Result<(), String> {
    // JSON lines on stdout: Container Insights and Application Insights pick them up as they are.
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let settings = PortalSettings::from_lookup(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path))?;
    if settings.customers == Identity::Dev {
        tracing::warn!("development identity: callers are trusted by header. Never expose this listener.");
    }
    let channel = Arc::new(Hosted {
        client: settings.client(),
        patience: settings.patience,
    });
    let master: Option<Arc<dyn ic_channels::customers::Master>> = match &settings.master {
        Some((base, scope)) => Some(Arc::new(ic_channels::customers::Hosted {
            base: base.clone(),
            scope: scope.clone(),
            credential: ic_azure::credential::AzureCredential::from_env().map_err(|e| format!("customer master credential: {e}"))?,
            http: ic_azure::http_client(),
        })),
        None => None,
    };
    let intake: Option<Arc<dyn ic_channels::disclosures::Intake>> = match &settings.intake {
        Some((base, scope)) => Some(Arc::new(ic_channels::disclosures::Hosted {
            base: base.clone(),
            scope: scope.clone(),
            credential: ic_azure::credential::AzureCredential::from_env().map_err(|e| format!("underwriting intake credential: {e}"))?,
            http: ic_azure::http_client(),
        })),
        None => None,
    };
    let documents: Option<Arc<dyn ic_channels::documents::Documents>> = match &settings.documents {
        Some((base, scope)) => Some(Arc::new(ic_channels::documents::Hosted {
            base: base.clone(),
            scope: scope.clone(),
            credential: ic_azure::credential::AzureCredential::from_env().map_err(|e| format!("documents credential: {e}"))?,
            http: ic_azure::http_client(),
        })),
        None => None,
    };
    let group: Option<Arc<dyn ic_channels::group::Group>> = match &settings.group {
        Some((base, scope)) => Some(Arc::new(ic_channels::group::Hosted {
            base: base.clone(),
            scope: scope.clone(),
            credential: ic_azure::credential::AzureCredential::from_env().map_err(|e| format!("group credential: {e}"))?,
            http: ic_azure::http_client(),
        })),
        None => None,
    };
    let libraries = ic_channels::wordings::SHIPPED
        .into_iter()
        .chain(settings.definitions.iter().map(|(file, source)| (file.as_str(), source.as_str())));
    let return_files = ic_channels::finance::SHIPPED_RETURNS
        .into_iter()
        .chain(settings.return_definitions.iter().map(|(file, source)| (file.as_str(), source.as_str())));
    let state = Arc::new(AppState {
        returns: Arc::new(ic_channels::finance::Returns::load(return_files).map_err(|e| format!("return definitions: {e}"))?),
        wordings: Arc::new(ic_channels::wordings::Wordings::load(libraries).map_err(|e| format!("definition libraries: {e}"))?),
        documents,
        group,
        master,
        intake,
        channel: channel.clone(),
        customers: settings.customers.auth(),
        workforce: settings.workforce.auth(),
        clock: Arc::new(wall_clock),
        regions: Live::new(settings.client(), settings.regions.clone()),
        ui: settings.ui.clone(),
        public_url: settings.public_url.clone(),
        complaints: Arc::new(forward::Hosted {
            client: settings.client(),
            module: ic_channels::complaints::MODULE,
            patience: settings.patience,
        }),
        claims: Arc::new(ic_channels::claims::Hosted {
            client: settings.client(),
            partitions: settings.partitions,
            patience: settings.patience,
        }),
        finance: Arc::new(forward::Hosted {
            client: settings.client(),
            module: ic_channels::finance::MODULE,
            patience: settings.patience,
        }),
        reinsurance: Arc::new(forward::Hosted {
            client: settings.client(),
            module: ic_channels::reinsurance::MODULE,
            patience: settings.patience,
        }),
        payments: Arc::new(forward::Hosted {
            client: settings.client(),
            module: ic_channels::payments::MODULE,
            patience: settings.patience,
        }),
        underwriting: Arc::new(forward::Hosted {
            client: settings.client(),
            module: ic_channels::underwriting::MODULE,
            patience: settings.patience,
        }),
        commission: Arc::new(forward::Hosted {
            client: settings.client(),
            module: forward::COMMISSION_MODULE,
            patience: settings.patience,
        }),
        pricing: Arc::new(forward::Hosted {
            client: settings.client(),
            module: ic_channels::pricing::MODULE,
            patience: settings.patience,
        }),
    });
    let beat = tokio::spawn(heartbeat(channel.clone(), settings.client(), settings.heartbeat, Arc::new(wall_clock)));
    let carry = tokio::spawn(forward(
        Arc::new(forward::Hosted {
            client: settings.client(),
            module: forward::COMMISSION_MODULE,
            patience: settings.patience,
        }),
        channel,
        settings.forward,
        Arc::new(wall_clock),
        settings.currency.clone(),
    ));
    let listener = tokio::net::TcpListener::bind(settings.listen)
        .await
        .map_err(|e| format!("bind {}: {e}", settings.listen))?;
    tracing::info!(listen = %settings.listen, nodes = settings.nodes.len(), "portal service listening");
    let served = axum::serve(listener, router(state)).with_graceful_shutdown(shutdown()).await;
    beat.abort();
    carry.abort();
    served.map_err(|e| e.to_string())
}
