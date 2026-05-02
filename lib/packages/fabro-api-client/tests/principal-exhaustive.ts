import type { Principal, PrincipalSystem, RunSpec } from "../src";

type AssertFalse<T extends false> = T;
type AssertExtends<T extends U, U> = true;
type IsAny<T> = 0 extends 1 & T ? true : false;

function assertNever(value: never): never {
  throw new Error(`Unexpected value: ${String(value)}`);
}

export function principalKind(principal: Principal): string {
  switch (principal.kind) {
    case "agent":
      return "agent";
    case "anonymous":
      return "anonymous";
    case "slack":
      return "slack";
    case "system":
      return systemKind(principal);
    case "user":
      return principal.login;
    case "webhook":
      return principal.delivery_id;
    case "worker":
      return principal.run_id;
    default:
      return assertNever(principal);
  }
}

export function systemKind(principal: PrincipalSystem): string {
  switch (principal.system_kind) {
    case "engine":
      return "engine";
    case "timeout":
      return "timeout";
    case "watchdog":
      return "watchdog";
    default:
      return assertNever(principal.system_kind);
  }
}

type Provenance = NonNullable<RunSpec["provenance"]>;
type Subject = NonNullable<Provenance["subject"]>;

type SubjectIsNotAny = AssertFalse<IsAny<Subject>>;
type SubjectExtendsPrincipal = AssertExtends<Subject, Principal>;
type PrincipalExtendsSubject = AssertExtends<Principal, Subject>;

const _principalSubject: Subject = {
  kind: "system",
  system_kind: "watchdog",
};

void (null as unknown as SubjectIsNotAny);
void (null as unknown as SubjectExtendsPrincipal);
void (null as unknown as PrincipalExtendsSubject);
void _principalSubject;
