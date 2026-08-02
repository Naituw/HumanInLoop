import { computed, ref } from "vue";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { fileToDataUrl } from "../../lib/theme";
import { readImageDataUrl } from "../../lib/ipc";
import type { ImageAttachment, InterjectAttachment } from "../../lib/types";

const IMAGE_FILE_EXT = /\.(png|jpe?g|gif|webp|bmp|heic|heif|tiff?|svg)$/i;

interface PathDraft {
  path: string;
  name: string;
  isImage: boolean;
  available: boolean;
}

interface PastedDraft {
  key: string;
  image: ImageAttachment;
}

function fileName(path: string): string {
  return path.replace(/\\/g, "/").split("/").pop() || path;
}

export function useInterjectAttachments() {
  const paths = ref<PathDraft[]>([]);
  const pasted = ref<PastedDraft[]>([]);
  const thumbnails = ref<Record<string, string>>({});
  const error = ref("");
  const busy = ref(false);
  let pastedSequence = 0;

  async function loadThumbnail(item: PathDraft): Promise<void> {
    if (!item.isImage || thumbnails.value[item.path] || !item.available) return;
    try {
      const data = await readImageDataUrl(item.path);
      if (paths.value.some((candidate) => candidate.path === item.path)) {
        thumbnails.value[item.path] = data;
      }
    } catch {
      // A readable non-previewable image remains a normal file chip.
    }
  }

  function reset(initial: InterjectAttachment[] = []): void {
    paths.value = initial.map((attachment) => ({
      path: attachment.path,
      name: attachment.name || fileName(attachment.path),
      isImage: attachment.isImage,
      available: attachment.available,
    }));
    pasted.value = [];
    thumbnails.value = {};
    error.value = "";
    for (const item of paths.value) void loadThumbnail(item);
  }

  function appendPaths(values: string[]): void {
    const known = new Set(paths.value.map((item) => item.path));
    for (const path of values) {
      if (!path || known.has(path)) continue;
      known.add(path);
      const item: PathDraft = {
        path,
        name: fileName(path),
        isImage: IMAGE_FILE_EXT.test(path),
        available: true,
      };
      paths.value.push(item);
      void loadThumbnail(item);
    }
    error.value = "";
  }

  async function chooseFiles(): Promise<void> {
    try {
      const result = await openDialog({ multiple: true, directory: false });
      appendPaths(Array.isArray(result) ? result : result ? [result] : []);
    } catch (cause) {
      error.value = String(cause);
    }
  }

  async function addPastedFiles(files: File[]): Promise<void> {
    const images = files.filter((file) => file.type.startsWith("image/"));
    if (!images.length) return;
    busy.value = true;
    try {
      const pastedAt = Date.now();
      const next = await Promise.all(
        images.map(async (file, index) => ({
          data: await fileToDataUrl(file),
          mediaType: file.type || "image/png",
          filename: file.name || `pasted-image-${pastedAt}-${index + 1}.png`,
        })),
      );
      pasted.value.push(
        ...next.map((image) => ({
          key: `pasted-${++pastedSequence}`,
          image,
        })),
      );
      error.value = "";
    } catch (cause) {
      error.value = String(cause);
    } finally {
      busy.value = false;
    }
  }

  async function onPaste(event: ClipboardEvent): Promise<boolean> {
    const items = event.clipboardData?.items;
    if (!items) return false;
    const files: File[] = [];
    for (let index = 0; index < items.length; index++) {
      const item = items[index];
      if (item.kind !== "file" || !item.type.startsWith("image/")) continue;
      const file = item.getAsFile();
      if (file) files.push(file);
    }
    if (!files.length) return false;
    event.preventDefault();
    await addPastedFiles(files);
    return true;
  }

  function removePath(path: string): void {
    const index = paths.value.findIndex((item) => item.path === path);
    if (index >= 0) paths.value.splice(index, 1);
    delete thumbnails.value[path];
  }

  const composerImages = computed(() => [
    ...paths.value.flatMap((item) => {
      const data = thumbnails.value[item.path];
      return data
        ? [{ key: `file:${item.path}`, data, filename: item.name, sourcePath: item.path }]
        : [];
    }),
    ...pasted.value.map((item) => ({
      key: item.key,
      data: item.image.data,
      filename: item.image.filename,
      pastedKey: item.key,
    })),
  ]);

  const composerFiles = computed(() =>
    paths.value
      .filter((item) => !thumbnails.value[item.path])
      .map((item) => ({
        key: item.path,
        path: item.path,
        name: item.name,
        available: item.available,
      })),
  );

  function removeComposerImage(index: number): void {
    const item = composerImages.value[index];
    if (!item) return;
    if ("sourcePath" in item && item.sourcePath) {
      removePath(item.sourcePath);
      return;
    }
    if ("pastedKey" in item && item.pastedKey) {
      const pastedIndex = pasted.value.findIndex((candidate) => candidate.key === item.pastedKey);
      if (pastedIndex >= 0) pasted.value.splice(pastedIndex, 1);
    }
  }

  function removeComposerFile(index: number): void {
    const item = composerFiles.value[index];
    if (item) removePath(item.path);
  }

  const hasAttachments = computed(() => paths.value.length > 0 || pasted.value.length > 0);
  const attachmentCount = computed(() => paths.value.length + pasted.value.length);

  return {
    error,
    busy,
    composerImages,
    composerFiles,
    filePaths: computed(() => paths.value.map((item) => item.path)),
    pastedImages: computed(() => pasted.value.map((item) => item.image)),
    hasAttachments,
    attachmentCount,
    reset,
    appendPaths,
    chooseFiles,
    onPaste,
    removeComposerImage,
    removeComposerFile,
  };
}
