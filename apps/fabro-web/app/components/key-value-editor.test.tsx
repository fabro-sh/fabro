import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { useState } from "react";
import TestRenderer, { act } from "react-test-renderer";

import { setupReactTestEnv } from "../lib/test-utils";
import { KeyValueEditor, entriesFromMap, mapFromEntries } from "./key-value-editor";

let teardownReactTestEnv: (() => void) | undefined;
const mountedRenderers: TestRenderer.ReactTestRenderer[] = [];

describe("key/value helpers", () => {
  test("mapFromEntries trims keys and drops blank keys", () => {
    expect(
      mapFromEntries([
        { key: " FOO ", value: "bar" },
        { key: "   ", value: "ignored" },
        { key: "BAZ", value: "qux" },
      ]),
    ).toEqual({ FOO: "bar", BAZ: "qux" });
  });

  test("entriesFromMap round-trips through mapFromEntries", () => {
    const map = { FOO: "bar", BAZ: "qux" };
    expect(mapFromEntries(entriesFromMap(map)))
      .toEqual(map);
  });
});

describe("KeyValueEditor", () => {
  beforeEach(() => {
    teardownReactTestEnv = setupReactTestEnv();
  });

  afterEach(() => {
    act(() => {
      for (const renderer of mountedRenderers.splice(0)) {
        renderer.unmount();
      }
    });
    teardownReactTestEnv?.();
    teardownReactTestEnv = undefined;
  });

  test("adds and removes rows", () => {
    let renderer: TestRenderer.ReactTestRenderer | undefined;

    function Host() {
      const [entries, setEntries] = useState([{ key: "FOO", value: "bar" }]);
      return (
        <KeyValueEditor
          entries={entries}
          onChange={setEntries}
          keyPlaceholder="KEY"
          valuePlaceholder="VALUE"
          addLabel="Add row"
        />
      );
    }

    act(() => {
      renderer = TestRenderer.create(<Host />);
    });
    mountedRenderers.push(renderer!);
    expect(renderer!.root.findAllByProps({ "aria-label": "Key" })).toHaveLength(1);

    const addButton = renderer!.root
      .findAllByType("button")
      .find((button) => button.props["aria-label"] === undefined);
    expect(addButton).toBeDefined();
    act(() => {
      addButton!.props.onClick();
    });
    expect(renderer!.root.findAllByProps({ "aria-label": "Key" })).toHaveLength(2);

    const removeButtons = renderer!.root.findAllByProps({ "aria-label": "Remove row" });
    act(() => {
      removeButtons[0].props.onClick();
    });
    expect(renderer!.root.findAllByProps({ "aria-label": "Key" })).toHaveLength(1);
  });
});
