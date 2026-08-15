import { expect, test } from "vitest";
import type { BrowseVolume } from "./api";
import { childPath, crumbPath, folderAt, nodeKey } from "./browse-paths";

const vol: BrowseVolume = {
  id: "label:SSD-A",
  label: "SSD-A",
  online: true,
  folders: [
    { path: "", children: ["ProjectX"], recursive_count: 2 },
    { path: "ProjectX", children: [], recursive_count: 2 },
  ],
};

test("a folder is found by its path, and the root by the empty one", () => {
  expect(folderAt(vol, "")?.children).toEqual(["ProjectX"]);
  expect(folderAt(vol, "ProjectX")?.recursive_count).toBe(2);
  expect(folderAt(vol, "Nope")).toBeNull();
});

test("a child of the root is its own name, with no leading slash", () => {
  expect(childPath("", "ProjectX")).toBe("ProjectX");
  expect(childPath("ProjectX", "B-Roll")).toBe("ProjectX/B-Roll");
});

test("a breadcrumb names everything up to and including itself", () => {
  const crumbs = ["ProjectX", "B-Roll", "Day1"];
  expect(crumbPath(crumbs, 0)).toBe("ProjectX");
  expect(crumbPath(crumbs, 1)).toBe("ProjectX/B-Roll");
  expect(crumbPath(crumbs, 2)).toBe("ProjectX/B-Roll/Day1");
});

test("no two different nodes can share a key", () => {
  // The pair the length prefix exists for. A volume id is `label:` and
  // whatever the drive is called, and nothing stops that from holding the
  // separator: joined on a slash, both of these spell `label:A/B/C`, and one
  // volume's folder would silently open — or close — another's.
  expect(nodeKey("label:A", "B/C")).not.toBe(nodeKey("label:A/B", "C"));
  // A volume's own root is not the same node as anything under it.
  expect(nodeKey("label:SSD-A", "")).not.toBe(nodeKey("label:SSD-A", "A"));
  // The same node is the same key, whoever asks.
  expect(nodeKey("label:SSD-A", "A/B")).toBe(nodeKey("label:SSD-A", "A/B"));
});
