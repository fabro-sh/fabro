import type { ServerSettings } from "@qltysh/fabro-api-client";
import { useServerSettings } from "../lib/queries";
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
  const settings = settingsQuery.data;

  return (
    <div className="space-y-6">
      <SettingsPageIntro description={DESCRIPTION} />
      {settings ? (
        <>
          <StorageRootPanel settings={settings} />
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

function StorageRootPanel({ settings }: { settings: ServerSettings }) {
  const { storage } = settings.server;
  return (
    <Panel title="Storage root">
      <Row title="Path" help="Filesystem path for run state and logs.">
        <Mono>{storage.root}</Mono>
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
