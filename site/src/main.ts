import "./style.css";

document.documentElement.classList.add("js");

// Resolve the newest published release so the download buttons never lag the
// truth. The HTML ships pointing at the releases page, so a failed fetch, a
// rate limit, or JS-off all degrade to a working link.
interface ReleaseAsset {
  name: string;
  browser_download_url: string;
}

interface Release {
  tag_name: string;
  assets: ReleaseAsset[];
}

async function resolveDownload(): Promise<void> {
  const buttons = [
    document.getElementById("download-btn"),
    document.getElementById("download-btn-2"),
  ].filter((el): el is HTMLAnchorElement => el instanceof HTMLAnchorElement);
  const note = document.getElementById("download-note");
  if (buttons.length === 0) return;

  try {
    const res = await fetch(
      "https://api.github.com/repos/statik/majestical/releases/latest",
      { headers: { Accept: "application/vnd.github+json" } },
    );
    if (!res.ok) return;
    const release = (await res.json()) as Release;
    const dmg = release.assets.find((a) => a.name.endsWith(".dmg"));
    if (!dmg) return;
    for (const button of buttons) {
      button.href = dmg.browser_download_url;
    }
    if (note) {
      note.textContent = `${release.tag_name} · Apache-2.0 · updates itself from GitHub releases`;
    }
  } catch {
    // Leave the releases-page links in place.
  }
}

function revealSections(): void {
  const sections = document.querySelectorAll<HTMLElement>(".section");
  if (!("IntersectionObserver" in window)) {
    for (const s of sections) s.classList.add("is-visible");
    return;
  }
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          entry.target.classList.add("is-visible");
          observer.unobserve(entry.target);
        }
      }
    },
    { rootMargin: "0px 0px -10% 0px" },
  );
  for (const s of sections) observer.observe(s);
}

void resolveDownload();
revealSections();
