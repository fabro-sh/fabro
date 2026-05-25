import type { AuthSessionUser } from "@qltysh/fabro-api-client";
import { useAuthMe } from "../lib/queries";
import {
  Mono,
  Muted,
  Panel,
  PanelSkeleton,
  Row,
  UrlValue,
} from "../components/settings-panel";

export default function ProfileOverview() {
  const { data: auth } = useAuthMe();

  if (!auth) {
    return (
      <div className="space-y-6">
        <PanelSkeleton />
        <PanelSkeleton />
      </div>
    );
  }

  const { user } = auth;

  return (
    <div className="space-y-6">
      <BasicsPanel user={user} />
      <IdentityPanel user={user} />
    </div>
  );
}

function BasicsPanel({ user }: { user: AuthSessionUser }) {
  return (
    <Panel title="Basics">
      <Row title="Name">
        <div className="flex items-center gap-3">
          <img
            alt=""
            src={user.avatarUrl}
            className="size-10 rounded-full outline -outline-offset-1 outline-line-strong"
          />
          <span>{user.name}</span>
        </div>
      </Row>
      <Row title="Username">
        <Mono>{user.login}</Mono>
      </Row>
      <Row title="Email">{user.email}</Row>
    </Panel>
  );
}

function IdentityPanel({ user }: { user: AuthSessionUser }) {
  return (
    <Panel title="Identity">
      <Row title="Issuer">
        {user.idpIssuer ? <Mono>{user.idpIssuer}</Mono> : <Muted>Not available</Muted>}
      </Row>
      <Row title="Subject">
        {user.idpSubject ? <Mono>{user.idpSubject}</Mono> : <Muted>Not available</Muted>}
      </Row>
      <Row title="Profile URL">
        <UrlValue url={user.userUrl} />
      </Row>
    </Panel>
  );
}
