import { createContext, useContext } from "react";

/** Lets a page switch to another page of the app. Outside the app shell it does nothing. */
export const NavContext = createContext<(id: string) => void>(() => {});

export function useNavigate(): (id: string) => void {
  return useContext(NavContext);
}
