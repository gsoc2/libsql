use std::sync::Arc;
use std::time::Duration;

use libsql_server::config::{AdminApiConfig, DbConfig, RpcClientConfig, RpcServerConfig};
use tokio::sync::Notify;

use crate::common::{
    http::Client,
    net::{SimServer, TestServer, TurmoilAcceptor, TurmoilConnector},
};

/// In this test, we first create a primary with a very small max_log_size, and then add a good
/// amount of data to it. This will cause the primary to create a bunch of snaphots a large enough
/// to prevent the replica from applying them all at once. We then start the replica, and check
/// that it replicates correctly to the primary's replicaton index.
#[test]
fn apply_partial_snapshot() {
    let mut sim = turmoil::Builder::new()
        .tcp_capacity(4096 * 30)
        .simulation_duration(Duration::from_secs(3600))
        .build();

    let prim_tmp = tempfile::tempdir().unwrap();
    let notify = Arc::new(Notify::new());

    sim.host("primary", {
        let prim_path = prim_tmp.path().to_path_buf();
        move || {
            let prim_path = prim_path.clone();
            async move {
                let primary = TestServer {
                    path: prim_path.into(),
                    db_config: DbConfig {
                        max_log_size: 1,
                        ..Default::default()
                    },
                    admin_api_config: Some(AdminApiConfig {
                        acceptor: TurmoilAcceptor::bind(([0, 0, 0, 0], 9090)).await.unwrap(),
                        connector: TurmoilConnector,
                        disable_metrics: true,
                    }),
                    rpc_server_config: Some(RpcServerConfig {
                        acceptor: TurmoilAcceptor::bind(([0, 0, 0, 0], 5050)).await.unwrap(),
                        tls_config: None,
                    }),
                    ..Default::default()
                };

                primary.start_sim(8080).await.unwrap();

                Ok(())
            }
        }
    });

    sim.host("replica", {
        let notify = notify.clone();
        move || {
            let notify = notify.clone();
            async move {
                let tmp = tempfile::tempdir().unwrap();
                let replica = TestServer {
                    path: tmp.path().to_path_buf().into(),
                    db_config: DbConfig {
                        max_log_size: 1,
                        ..Default::default()
                    },
                    admin_api_config: Some(AdminApiConfig {
                        acceptor: TurmoilAcceptor::bind(([0, 0, 0, 0], 9090)).await.unwrap(),
                        connector: TurmoilConnector,
                        disable_metrics: true,
                    }),
                    rpc_client_config: Some(RpcClientConfig {
                        remote_url: "http://primary:5050".into(),
                        tls_config: None,
                        connector: TurmoilConnector,
                    }),
                    ..Default::default()
                };

                notify.notified().await;
                replica.start_sim(8080).await.unwrap();

                Ok(())
            }
        }
    });

    sim.client("client", async move {
        let primary = libsql::Database::open_remote_with_connector(
            "http://primary:8080",
            "",
            TurmoilConnector,
        )
        .unwrap();
        let conn = primary.connect().unwrap();
        conn.execute("CREATE TABLE TEST (x)", ()).await.unwrap();
        // we need a sufficiently large snapshot for the test. Before the fix, 5000 insert would
        // trigger an infinite loop.
        for _ in 0..5000 {
            conn.execute("INSERT INTO TEST VALUES (randomblob(6000))", ())
                .await
                .unwrap();
        }

        let client = Client::new();
        let resp = client
            .get("http://primary:9090/v1/namespaces/default/stats")
            .await
            .unwrap();
        let stats = resp.json_value().await.unwrap();
        let primary_replication_index = stats["replication_index"].as_i64().unwrap();

        // primary is setup, time to start replica
        notify.notify_waiters();

        let client = Client::new();

        // wait for replica to start up
        while client.get("http://replica:8080/").await.is_err() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        loop {
            let resp = client
                .get("http://replica:9090/v1/namespaces/default/stats")
                .await
                .unwrap();
            let stats = resp.json_value().await.unwrap();
            let replication_index = &stats["replication_index"];
            if !replication_index.is_null() {
                if replication_index.as_i64().unwrap() == primary_replication_index {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }

        Ok(())
    });

    sim.run().unwrap();
}
