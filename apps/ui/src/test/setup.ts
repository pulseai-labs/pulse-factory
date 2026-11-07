import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// React tells the test environment that updates run inside `act`.
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

afterEach(() => {
  cleanup();
  try {
    window.localStorage.clear();
  } catch {
    // No storage in this environment: nothing to clear.
  }
});
