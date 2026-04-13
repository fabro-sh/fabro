import { redirect } from "react-router";
import { getAuthMe } from "../api";

export async function loader() {
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
