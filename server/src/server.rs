use crate::{
    global::Global,
    id_manager::IDManager,
    route::Endpoint,
    router::Router,
    tcp::Handler,
    types::{GlobalVars, ReadyIndicator},
    BanManager, ConfigManager, Connection, Result, SessionList, StratumServerBuilder,
};
use extended_primitives::Buffer;
use futures::StreamExt;
use rlimit::Resource;
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

pub struct StratumServer<State, CState>
where
    State: Clone,
    CState: Default + Clone,
{
    pub(crate) id: u8,
    pub(crate) listen_address: SocketAddr,
    pub(crate) listener: TcpListenerStream,
    pub(crate) state: State,
    pub(crate) session_list: SessionList<CState>,
    pub(crate) ban_manager: BanManager,
    pub(crate) config_manager: ConfigManager,
    pub(crate) router: Arc<Router<State, CState>>,
    pub(crate) session_id_manager: IDManager,
    pub(crate) cancel_token: CancellationToken,
    pub(crate) global_thread_list: Vec<JoinHandle<()>>,
    pub(crate) ready_indicator: ReadyIndicator,
    pub(crate) shutdown_message: Option<Buffer>,
    #[cfg(feature = "api")]
    pub(crate) api: crate::api::Api,
}

impl<State, CState> StratumServer<State, CState>
where
    State: Clone + Send + Sync + 'static,
    CState: Default + Clone + Send + Sync + 'static,
{
    pub fn builder(state: State, server_id: u8) -> StratumServerBuilder<State, CState> {
        StratumServerBuilder::new(state, server_id)
    }

    pub fn add(&mut self, method: &str, ep: impl Endpoint<State, CState>) {
        let router = Arc::get_mut(&mut self.router)
            .expect("Registering routes is not possible after the Server has started");
        router.add(method, ep);
    }

    pub fn global(&mut self, _global_name: &str, ep: impl Global<State, CState>) {
        let state = self.state.clone();
        let session_list = self.session_list.clone();
        let cancel_token = self.get_cancel_token();

        let handle = tokio::spawn(async move {
            tokio::select! {
                _res = ep.call(state, session_list) => {
                    //@todo call does not return an Error. It should!
                    // if let Err(e) = res {
                    //     //@todo more indepth, lots of stuff to include here.
                    //     error!("Global thread failed.");
                    // }
                }
                _ = cancel_token.cancelled() => {
                    //@todo
                    info!("Global thread XYZ is shutting down from shutdown message.");
                }

            }
            // let call = ep.call(state, connection_list).timeout_at(stop_token);
            // match call.await {
            //     Ok(()) => {}
            //     Err(_e) => {
            //         //@todo we can't do any of this until Tokio stablizes these APIs
            //         //@todo - This will only be relevant for when we have results returned by
            //         //Globals because currently this block will only be called if the stop
            //         //token is revoked. Re-enable this when that is implemented.
            //         //2. We should probably have a config setting that asks if we want to nuke everything if a
            //         //   global falls. I don't know if we should automatically nuke everything, but it should
            //         //   be a setting that is available. Maybe it's available on a per-global basis, but I
            //         //   think it's something we should absolutely know about.
            //         //   @
            //         // warn!(
            //         //     "Global thread id: {} name: {} was unexpected closed by the error: {}",
            //         //     async_std::task::current().id(),
            //         //     async_std::task::current().name().unwrap_or(""),
            //         //     e
            //         // );
            //         // info!(
            //         //     "Global thread id: {} name: {} was closed",
            //         //     async_std::task::current().id(),
            //         //     async_std::task::current().name().unwrap_or("")
            //         // );
            //     }
            // }
        });

        // let handle = async_std::task::Builder::new()
        //     .name(global_name.to_string())
        //     .spawn(async move {
        //         let call = ep.call(state, connection_list).timeout_at(stop_token);
        //         match call.await {
        //             Ok(()) => {}
        //             Err(_e) => {
        //                 //@todo - This will only be relevant for when we have results returned by
        //                 //Globals because currently this block will only be called if the stop
        //                 //token is revoked. Re-enable this when that is implemented.
        //                 //2. We should probably have a config setting that asks if we want to nuke everything if a
        //                 //   global falls. I don't know if we should automatically nuke everything, but it should
        //                 //   be a setting that is available. Maybe it's available on a per-global basis, but I
        //                 //   think it's something we should absolutely know about.
        //                 //   @
        //                 // warn!(
        //                 //     "Global thread id: {} name: {} was unexpected closed by the error: {}",
        //                 //     async_std::task::current().id(),
        //                 //     async_std::task::current().name().unwrap_or(""),
        //                 //     e
        //                 // );
        //                 info!(
        //                     "Global thread id: {} name: {} was closed",
        //                     async_std::task::current().id(),
        //                     async_std::task::current().name().unwrap_or("")
        //                 );
        //             }
        //         };
        //     })
        //     //@todo switch this to expect
        //     .unwrap();

        self.global_thread_list.push(handle);
    }

    async fn handle_incoming(&mut self) -> Result<()> {
        info!("Listening on {}", &self.listen_address);

        while let Some(stream) = self.listener.next().await {
            //@todo we might actually want access to this error though.
            let Ok(stream) = stream else {
                continue;
            };

            //@todo might make sense to wrap this in ConnectionID, so that we can implement Valuable
            //from tracing so that we can directly print the IDs
            //@todo we could also move this to handler? Idk also works here. Depends if we need it
            //in connection or not.
            let id = Uuid::new_v4();
            let child_token = self.get_cancel_token();

            //@todo for this error, make sure we print it in Connection.
            let Ok(connection) = Connection::new(id, stream, child_token.clone()) else {
                continue;
            };

            let handler = Handler {
                id,
                ban_manager: self.ban_manager.clone(),
                id_manager: self.session_id_manager.clone(),
                session_list: self.session_list.clone(),
                router: self.router.clone(),
                state: self.state.clone(),
                connection_state: CState::default(),
                config_manager: self.config_manager.clone(),
                cancel_token: child_token,
                global_vars: GlobalVars::new(self.id),
                connection,
            };

            //@todo here is how we should do this.
            //We should pass these threads to a ThreadManager, and include some kind of key with them?
            //Connection::new() should generate a UUID rather than Session. Then we can save these via
            //UUID, and when they shutdown, the server can ping threadManager to clean it up. If they
            //aren't able to do that, we will also just have periodic prunings of the threads to ensure
            //all is well.
            tokio::spawn(async move {
                if let Err(err) = handler.run().await {
                    error!(cause = ?err, "connection error");
                }
            });
        }

        Ok(())
    }

    pub async fn start(&mut self) -> Result<()> {
        init()?;

        let cancel_token = self.cancel_token.clone();

        #[cfg(feature = "api")]
        let api_handle = self.api.run(cancel_token.clone())?;

        tokio::select! {
            res = self.handle_incoming() => {
                if let Err(err) = res {
                    error!(cause = %err, "failed to accept");
                };
            },
            _ = cancel_token.cancelled() => {}
        }

        let start = Instant::now();

        //Session Shutdowns
        {
            self.session_list
                .shutdown_msg(self.shutdown_message.clone())?;

            let mut backoff = 1;
            loop {
                let connected_miners = self.session_list.len();
                if connected_miners == 0 {
                    break;
                }

                if backoff > 64 {
                    warn!("{connected_miners} remaining, force shutting down now");
                    self.session_list.shutdown();
                    break;
                }

                info!("Waiting for all miners to disconnect, {connected_miners} remaining");
                tokio::time::sleep(Duration::from_secs(backoff)).await;

                backoff *= 2;
            }
        }

        let global_thread_list = self.global_thread_list.drain(..);

        //@TODO make this parrallel.
        info!("Awaiting for all current globals to complete");
        for thread in global_thread_list {
            //@todo handle this better just report the error I think.
            if let Err(err) = thread.await {
                error!(cause = %err, "Global thread failed to shut down gracefully.");
            }
        }

        #[cfg(feature = "api")]
        {
            info!("Waiting for Api handler to finish");
            //@todo report the errors here
            if let Err(err) = api_handle.await {
                error!(cause = %err, "API failed to shut down gracefully.");
            }
        }

        //@todo lets get some better parsing here. Seconds and NS would be great
        info!("Shutdown complete in {} ms", start.elapsed().as_millis());

        Ok(())
    }

    pub fn get_ready_indicator(&self) -> ReadyIndicator {
        self.ready_indicator.create_new()
    }

    // #[cfg(test)]
    pub fn get_miner_list(&self) -> SessionList<CState> {
        self.session_list.clone()
    }

    pub fn get_cancel_token(&self) -> CancellationToken {
        self.cancel_token.child_token()
    }

    pub fn get_address(&self) -> SocketAddr {
        self.listen_address
    }

    pub fn get_ban_manager(&self) -> BanManager {
        self.ban_manager.clone()
    }

    #[cfg(feature = "api")]
    pub fn get_api_address(&self) -> SocketAddr {
        self.api.listen_address()
    }
}

fn init() -> Result<()> {
    info!("Initializing...");

    //Initalize the recorder
    init_metrics_recorder();

    //@todo let's wrap this to make sure it's aboe what we need otherwise adjust.
    //Check that the system will support what we need.
    let (hard, soft) = rlimit::getrlimit(Resource::NOFILE)?;

    info!("Current Ulimit is set to {hard} hard limit, {soft} soft limit");

    info!("Initialization Complete");

    Ok(())
}

//Initalizes the prometheus metrics recorder.
pub fn init_metrics_recorder() {
    // let (recorder, _) = PrometheusBuilder::new().build().unwrap();

    ////@todo this is breaking.
    // metrics::set_boxed_recorder(Box::new(recorder));
}

// impl<State: Clone + Send + Sync + 'static, CState: Default + Clone + Send + Sync + 'static> Drop
//     for StratumServer<State, CState>
// {
//     fn drop(&mut self) {
//         info!("Dropping StratumSever with data `{}`!", self.host);
//     }
// }
//

//@todo
// #[cfg(test)]
// mod tests {
//
//     #[derive(Clone)]
//     pub struct State {}
//
//     pub async fn handle_auth(
//         req: StratumRequest<State>,
//         _connection: Arc<Session<ConnectionState>>,
//     ) -> Result<bool> {
//         let state = req.state();
//
//         let login = state.auth.login().await;
//
//         Ok(login)
//     }
//
//     #[tokio::test]
//     async fn test_server_add() {
//         let builder = StratumServer::builder(state, 1)
//             .with_host("0.0.0.0")
//             .with_port(0);
//
//         let mut server = builder.build().await?;
//
//         let address = server.get_address();
//
//         server.add("auth", handle_auth);
//     }
// }
