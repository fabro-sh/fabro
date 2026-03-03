import { createCookieSessionStorage, redirect } from "react-router";

interface SessionData {
  githubLogin: string;
  name: string;
  email: string;
  avatarUrl: string;
  accessToken: string;
}

function getSessionStorage() {
  const secret = process.env.SESSION_SECRET;
  if (!secret) {
    throw new Error("SESSION_SECRET is not set");
  }
  return createCookieSessionStorage<SessionData>({
    cookie: {
      name: "__arc_session",
      httpOnly: true,
      sameSite: "lax",
      secure: process.env.NODE_ENV === "production",
      secrets: [secret],
      path: "/",
    },
  });
}

export async function getSession(request: Request) {
  const storage = getSessionStorage();
  return storage.getSession(request.headers.get("Cookie"));
}

export async function commitSession(session: Awaited<ReturnType<typeof getSession>>) {
  const storage = getSessionStorage();
  return storage.commitSession(session);
}

export async function destroySession(session: Awaited<ReturnType<typeof getSession>>) {
  const storage = getSessionStorage();
  return storage.destroySession(session);
}

export async function getUser(request: Request) {
  const session = await getSession(request);
  const githubLogin = session.get("githubLogin");
  if (!githubLogin) return null;
  return {
    githubLogin,
    name: session.get("name") ?? githubLogin,
    email: session.get("email") ?? "",
    avatarUrl: session.get("avatarUrl") ?? "",
  };
}

export async function requireUser(request: Request) {
  const user = await getUser(request);
  if (!user) throw redirect("/auth/login");
  return user;
}
