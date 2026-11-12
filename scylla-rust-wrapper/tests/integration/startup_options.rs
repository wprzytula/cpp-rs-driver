use std::collections::HashMap;
use std::sync::Arc;

use scylla_cql::frame::request::options;
use scylla_cql::frame::types;
use scylla_proxy::{
    Condition, ProxyError, Reaction as _, RequestFrame, RequestOpcode, RequestReaction,
    RequestRule, ResponseFrame, RunningProxy, WorkerError,
};
use scylladb::{
    api::{
        cluster::{
            cass_cluster_free, cass_cluster_new, cass_cluster_set_contact_points,
            cass_cluster_set_driver_config_reporting,
        },
        error::CassError,
        session::{cass_session_connect, cass_session_free, cass_session_new},
    },
    argconv::CassStrNulTerminated,
    types::{cass_bool_t, cass_false},
};
use tokio::sync::mpsc;
use tracing::instrument::WithSubscriber as _;

use crate::utils::{
    assert_cass_error_eq, cass_future_wait_check_and_free, mock_init_rules,
    proxy_uris_to_contact_points, setup_tracing, test_with_3_node_dry_mode_cluster,
};

/// Connects a session to a 3-node dry-mode proxy and returns the options of
/// the STARTUP frames that the driver sent until `cass_session_connect()`
/// completed.
///
/// If `driver_config_reporting` is `Some`, the test calls
/// `cass_cluster_set_driver_config_reporting()` with that value before it connects.
///
/// The returned frames include the frame of the first control connection and
/// the frame of one pool connection for each node:
/// - The session waits for these connections before it completes the connect.
/// - The proxy sends the feedback before it replies to STARTUP.
///
/// The returned frames can also include frames of more control connections.
/// The dry-mode proxy does not reply to the metadata queries. Thus, the driver
/// drops the control connection after the first metadata fetch, and it opens
/// a new one immediately.
async fn startup_options_of_new_session(
    driver_config_reporting: Option<cass_bool_t>,
) -> Vec<HashMap<String, String>> {
    let (startup_tx, mut startup_rx) = mpsc::unbounded_channel();

    // The proxy applies only the first rule that matches. Thus, this rule must
    // reply to STARTUP itself, and it must come before `mock_init_rules()`.
    let rules = || {
        std::iter::once(RequestRule(
            Condition::RequestOpcode(RequestOpcode::Startup),
            RequestReaction::forge_response(Arc::new(|frame: RequestFrame| {
                ResponseFrame::forged_ready(frame.params)
            }))
            .with_feedback_when_performed(startup_tx.clone()),
        ))
        .chain(mock_init_rules())
    };

    let (options_tx, options_rx) = std::sync::mpsc::channel();

    let res = test_with_3_node_dry_mode_cluster(
        rules,
        move |proxy_uris: [String; 3], proxy: RunningProxy| {
            unsafe {
                let mut cluster_raw = cass_cluster_new();
                let contact_points = proxy_uris_to_contact_points(proxy_uris);
                assert_cass_error_eq(
                    cass_cluster_set_contact_points(
                        cluster_raw.borrow_mut(),
                        CassStrNulTerminated::from_raw(contact_points.as_ptr()),
                    ),
                    CassError::CASS_OK,
                );
                if let Some(enabled) = driver_config_reporting {
                    cass_cluster_set_driver_config_reporting(cluster_raw.borrow_mut(), enabled);
                }

                let session_raw = cass_session_new();
                cass_future_wait_check_and_free(cass_session_connect(
                    session_raw.borrow(),
                    cluster_raw.borrow().into_c_const(),
                ));

                let mut startup_options = Vec::new();
                while let Ok((frame, _shard)) = startup_rx.try_recv() {
                    startup_options.push(types::read_string_map(&mut &*frame.body).unwrap());
                }
                options_tx.send(startup_options).unwrap();

                cass_session_free(session_raw);
                cass_cluster_free(cluster_raw);
            }

            proxy
        },
    )
    .with_current_subscriber()
    .await;

    match res {
        Ok(()) => (),
        Err(ProxyError::Worker(WorkerError::DriverDisconnected(_))) => (),
        Err(err) => panic!("{}", err),
    }

    let startup_options = options_rx.recv().unwrap();

    // 1 control connection and 1 pool connection for each of the 3 nodes.
    assert!(
        startup_options.len() >= 4,
        "expected at least 4 STARTUP frames, got {}",
        startup_options.len()
    );

    // The report setting has no effect on SESSION_ID.
    let session_ids = startup_options
        .iter()
        .map(|opts| {
            opts.get(options::SESSION_ID)
                .expect("STARTUP frame without a SESSION_ID option")
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        session_ids.len(),
        1,
        "the connections of one session sent different SESSION_ID values"
    );

    startup_options
}

#[tokio::test]
#[ntest::timeout(30000)]
async fn driver_config_is_reported_by_default() {
    setup_tracing();

    let startup_options = startup_options_of_new_session(None).await;

    let reports = startup_options
        .iter()
        .filter_map(|opts| opts.get(options::DRIVER_CONFIG))
        .collect::<Vec<_>>();
    assert!(
        !reports.is_empty(),
        "no STARTUP frame has a DRIVER_CONFIG option"
    );
    assert!(
        reports.iter().all(|report| !report.is_empty()),
        "a STARTUP frame has an empty DRIVER_CONFIG option"
    );
    // Only the control connection sends the report. The pool connections do not.
    assert!(
        reports.len() < startup_options.len(),
        "all STARTUP frames have a DRIVER_CONFIG option"
    );
}

#[tokio::test]
#[ntest::timeout(30000)]
async fn driver_config_is_not_reported_when_disabled() {
    setup_tracing();

    let startup_options = startup_options_of_new_session(Some(cass_false)).await;

    assert!(
        startup_options
            .iter()
            .all(|opts| !opts.contains_key(options::DRIVER_CONFIG)),
        "a session with the report disabled sent a DRIVER_CONFIG option"
    );
}
