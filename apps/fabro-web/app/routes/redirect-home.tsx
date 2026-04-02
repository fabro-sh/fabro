import { redirect } from "react-router";
import { getAuthMe, getSetupStatus } from "../api";

export async function loader() {
  const setup = await getSetupStatus();
  if (!setup.configured) {
    return redirect("/setup");
  }

  try {
    await getAuthMe();
  } catch (error) {
    if (error instanceof Response && error.status === 401) {
      return redirect("/login");
    }
    throw error;
  }

  return redirect("/start");
}

export default function RedirectHome() {
  return null;
}
