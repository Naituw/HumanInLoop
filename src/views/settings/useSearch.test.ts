import { mount } from "@vue/test-utils";
import { defineComponent, nextTick, ref } from "vue";
import { describe, expect, it } from "vitest";
import { i18n } from "../../i18n";
import type { AppConfig } from "../../lib/types";
import type { Tab } from "./context";
import { useSettingsSearch } from "./useSearch";

describe("useSettingsSearch", () => {
  it("indexes the Pi integration card", async () => {
    i18n.global.locale.value = "en";
    let search!: ReturnType<typeof useSettingsSearch>;
    const harness = defineComponent({
      setup() {
        search = useSettingsSearch({
          config: ref<AppConfig | null>(null),
          activeTab: ref<Tab>("general"),
        });
        return () => null;
      },
    });
    const wrapper = mount(harness, { global: { plugins: [i18n] } });

    search.searchQuery.value = "pi";
    await nextTick();

    expect(search.searchResults.value).toContainEqual({
      tab: "integration",
      title: "Pi",
      extra: ["Agent"],
    });

    search.searchQuery.value = "lifecycle tracking";
    await nextTick();
    expect(search.searchResults.value).toContainEqual({
      tab: "integration",
      title: "Lifecycle tracking",
      extra: expect.any(Array),
    });
    wrapper.unmount();
  });
});
