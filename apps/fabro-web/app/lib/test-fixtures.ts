import type { Principal } from "@qltysh/fabro-api-client";

export const TEST_PRINCIPAL: Principal = {
  kind:        "user",
  identity:    { issuer: "fabro:test", subject: "test-user" },
  login:       "test",
  auth_method: "dev_token",
};
