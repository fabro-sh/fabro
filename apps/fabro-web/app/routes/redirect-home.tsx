import { Navigate } from "react-router";
import { ApiError } from "../lib/api-client";
import { useAuthMe } from "../lib/queries";

export default function RedirectHome() {
  const { data, error } = useAuthMe();

  if (data) {
    return <Navigate to="/runs" replace />;
  }

  if (error instanceof ApiError && error.status === 401) {
    return <Navigate to="/login" replace />;
  }

  return null;
}
