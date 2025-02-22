// Copyright 2022. The Tari Project
//
// Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
// following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
// disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
// following disclaimer in the documentation and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
// products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
// INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
// USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//

use anyhow::Error;
use async_trait::async_trait;
use tari_base_node_grpc_client::{grpc, BaseNodeGrpcClient};
use tari_launchpad_protocol::container::TaskProgress;
use tari_sdm::{
    ids::{ManagedTask, TaskId},
    image::{
        checker::{CheckerContext, CheckerEvent, ContainerChecker},
        Args,
        Envs,
        ManagedContainer,
        Mounts,
        Networks,
        Ports,
        Volumes,
    },
};

use super::{
    sync_progress::SyncProgress,
    Tor,
    BLOCKCHAIN_PATH,
    BLOCKCHAIN_VOLUME,
    DEFAULT_REGISTRY,
    GENERAL_VOLUME,
    VAR_TARI_PATH,
};
use crate::resources::{
    config::{ConnectionSettings, LaunchpadConfig, LaunchpadInnerEvent, LaunchpadProtocol},
    networks::LocalNet,
    volumes::SharedVolume,
};

#[derive(Debug, Default)]
pub struct TariBaseNode {
    settings: Option<ConnectionSettings>,
}

impl ManagedTask for TariBaseNode {
    fn id() -> TaskId {
        "Base Node".into()
    }

    fn deps() -> Vec<TaskId> {
        vec![LocalNet::id(), SharedVolume::id(), Tor::id()]
    }
}

impl ManagedContainer for TariBaseNode {
    type Protocol = LaunchpadProtocol;

    fn registry(&self) -> &str {
        DEFAULT_REGISTRY
    }

    fn image_name(&self) -> &str {
        "tari_base_node"
    }

    fn reconfigure(&mut self, config: Option<&LaunchpadConfig>) -> Option<bool> {
        self.settings = ConnectionSettings::try_extract(config?);
        let session = &self.settings.as_ref()?.session;
        Some(session.all_active || session.base_layer_active || session.base_node_active)
    }

    fn checker(&mut self) -> Box<dyn ContainerChecker<LaunchpadProtocol>> {
        Box::new(Checker::new())
    }

    fn args(&self, args: &mut Args) {
        args.set("--log-config", "/var/tari/config/log4rs.yml");
        args.flag("-n");
        args.set("--watch", "status");
    }

    fn envs(&self, envs: &mut Envs) {
        if let Some(settings) = self.settings.as_ref() {
            settings.add_common(envs);
            settings.add_tor(envs);
            // envs.set("WAIT_FOR_TOR", 10);
            envs.set(
                "TARI_BASE_NODE__DATA_DIR",
                format!("/blockchain/{}", settings.tari_network.lower_case()),
            );
        }
        envs.set("APP_NAME", "base_node");
    }

    fn ports(&self, ports: &mut Ports) {
        ports.add(18_142);
        ports.add(18_189);
    }

    fn networks(&self, networks: &mut Networks) {
        networks.add("base_node", LocalNet::id());
    }

    fn volumes(&self, volumes: &mut Volumes) {
        volumes.add(GENERAL_VOLUME);
        volumes.add(BLOCKCHAIN_VOLUME);
    }

    fn mounts(&self, mounts: &mut Mounts) {
        if let Some(settings) = self.settings.as_ref() {
            // TODO: Avoid using display here
            mounts.bind_path(settings.data_directory.display(), VAR_TARI_PATH);
            mounts.add_volume(SharedVolume::id(), BLOCKCHAIN_PATH);
        }
    }
}

struct Checker {
    progress: SyncProgress,
    identity_sent: bool,
}

impl Checker {
    fn new() -> Self {
        let progress = SyncProgress::new(0, 100);
        Self {
            progress,
            identity_sent: false,
        }
    }
}

#[async_trait]
impl ContainerChecker<LaunchpadProtocol> for Checker {
    async fn on_interval(&mut self, ctx: &mut CheckerContext<LaunchpadProtocol>) -> Result<(), Error> {
        // TODO: Keep the client
        let mut client = BaseNodeGrpcClient::connect("http://127.0.0.1:18142").await?;

        if !self.identity_sent {
            let identity = client.identify(grpc::Empty {}).await?.into_inner().try_into()?;
            let event = LaunchpadInnerEvent::IdentityReady(identity);
            ctx.notify(event)?;
            self.identity_sent = true;
        }

        let response = client.get_sync_progress(grpc::Empty {}).await?.into_inner();
        log::trace!("Sync progress: {:?}", response);
        let done = matches!(response.state(), tari_app_grpc::tari_rpc::SyncState::Done);
        self.progress.update(response);
        let info = self.progress.progress_info();
        log::trace!("Progress updated !common::progress={}", info.block_progress);
        let progress = TaskProgress {
            pct: info.block_progress as u8,
            stage: "Syncing blockchain...".into(),
        };
        ctx.report(CheckerEvent::Progress(progress)).ok();
        if done {
            ctx.report(CheckerEvent::Ready).ok();
        }

        // let current = progress.local_height;
        // let total = progress.tip_height;
        // let pct = current as f32 / total as f32 * 100.0;
        // ctx.report_progress(CheckerEvent::Progress(pct as u8)).ok();
        // if current == total && total != 0 {
        // ctx.report_progress(CheckerEvent::Ready).ok();
        // }
        Ok(())
    }
}
