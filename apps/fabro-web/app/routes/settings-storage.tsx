import type {
  ServerSettings,
  SystemDiskResources,
} from "@qltysh/fabro-api-client";
import { formatBytesAsMemory } from "../lib/format";
import { useServerSettings, useSystemResources } from "../lib/queries";
import {
  Mono,
  ObjectStoreRows,
  Panel,
  PanelSkeleton,
  Row,
  SettingsPageIntro,
} from "../components/settings-panel";

export function meta() {
  return [{ title: "Storage — Fabro" }];
}

const DESCRIPTION = (
  <>
    Filesystem and object store locations for run state, the embedded database,
    and artifacts. Edit via{" "}
    <code className="font-mono text-fg-2">settings.toml</code>; changes take
    effect on the next server restart.
  </>
);

export default function SettingsStorage() {
  const settingsQuery = useServerSettings();
  const resourcesQuery = useSystemResources();
  const settings = settingsQuery.data;
  const resources = resourcesQuery.data;

  return (
    <div className="space-y-6">
      <SettingsPageIntro description={DESCRIPTION} />
      {settings && resources ? (
        <>
          <StorageRootPanel settings={settings} disk={resources.disk} />
          <SlateDbPanel settings={settings} />
          <ArtifactsPanel settings={settings} />
        </>
      ) : (
        <>
          <PanelSkeleton />
          <PanelSkeleton />
          <PanelSkeleton />
        </>
      )}
    </div>
  );
}

function StorageRootPanel({
  settings,
  disk,
}: {
  settings: ServerSettings;
  disk: SystemDiskResources;
}) {
  const { storage } = settings.server;
  return (
    <Panel title="Storage root">
      <Row title="Path" help="Filesystem path for run state and logs.">
        <Mono>{storage.root}</Mono>
      </Row>
      <Row title="Fabro managed" help="Bytes currently tracked under Fabro storage.">
        {formatBytesAsMemory(disk.fabro_managed_bytes, 0)}
      </Row>
      <Row title="Reclaimable" help="Bytes Fabro can reclaim by pruning inactive data.">
        {formatBytesAsMemory(disk.fabro_reclaimable_bytes, 0)}
      </Row>
    </Panel>
  );
}

function SlateDbPanel({ settings }: { settings: ServerSettings }) {
  const { slatedb } = settings.server;
  return (
    <Panel title="SlateDB">
      <ObjectStoreRows store={slatedb.store} prefix={slatedb.prefix} />
    </Panel>
  );
}

function ArtifactsPanel({ settings }: { settings: ServerSettings }) {
  const { artifacts } = settings.server;
  return (
    <Panel title="Artifacts">
      <ObjectStoreRows store={artifacts.store} prefix={artifacts.prefix} />
    </Panel>
  );
}
