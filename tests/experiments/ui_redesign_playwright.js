// Run with Playwright MCP browser_run_code_unsafe({ filename: this absolute path }).
// The real Svelte UI uses its explicit token-free development adapter.
async (page) => {
  const base = "http://127.0.0.1:5173/";
  const checks = [];
  const errors = [];
  page.on("pageerror", error => errors.push(String(error)));
  const wait = predicate => page.waitForFunction(predicate);
  const snapshot = () => page.evaluate(() => window.__SYNCHROME_PREVIEW__.snapshot());
  const check = (name, passed) => {
    checks.push({ name, passed: !!passed });
    if (!passed) throw new Error(name);
  };
  const scenario = async name => {
    await page.goto(`${base}?preview=${name}`);
    await wait(() => !!window.__SYNCHROME_PREVIEW__ && !document.querySelector(".boot"));
  };
  const nav = async name => page.getByTestId(`nav-${name}`).click();
  const modal = () => page.locator("dialog[open]");
  const button = name => modal().getByRole("button", { name, exact: true });
  const profile = async name => {
    await nav("profiles");
    await page.getByRole("button", { name: `Open profile ${name}`, exact: true }).click();
  };
  const chooseLocal = async prefix => {
    const value = await page.locator("#local-profile option").evaluateAll((options, prefix) =>
      options.find(option => !option.disabled && option.textContent.startsWith(prefix))?.value, prefix);
    if (!value) throw new Error(`Fixture browser unavailable: ${prefix}`);
    await page.locator("#local-profile").selectOption(value);
  };
  const run = async (name, callback) => {
    try { await callback(); }
    catch (error) { checks.push({ name: `${name} completed`, passed: false, detail: String(error) }); }
  };

  await page.setViewportSize({ width: 1100, height: 780 });
  await run("Auth and first entry", async () => {
    await scenario("auth");
    check("Auth exposes labelled fields and password autocomplete",
      await page.getByLabel("Email", { exact: true }).count() === 1 &&
      await page.getByLabel("Password", { exact: true }).getAttribute("autocomplete") === "current-password");
    await page.getByLabel("Email", { exact: true }).fill("invalid");
    await page.getByLabel("Password", { exact: true }).fill("fixture-password");
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await page.getByText("Enter a valid email address.", { exact: true }).waitFor();
    check("Invalid email focuses the email field", await page.getByLabel("Email", { exact: true }).evaluate(el => el === document.activeElement));
    await page.getByRole("button", { name: "Show password", exact: true }).click();
    check("Password visibility is explicit and reversible", await page.getByLabel("Password", { exact: true }).getAttribute("type") === "text");
    await page.getByRole("button", { name: "Hide password", exact: true }).click();
    await page.getByLabel("Email", { exact: true }).fill("review@example.test");
    await page.evaluate(() => window.__SYNCHROME_PREVIEW__.failNext("sign_in", "InvalidCredentials"));
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await page.locator(".auth-error").waitFor();
    check("Auth failure retains values and gives human feedback",
      await page.getByLabel("Password", { exact: true }).inputValue() === "fixture-password" &&
      (await page.locator(".auth-error").innerText()).includes("password"));
    await page.getByRole("button", { name: "Create an account", exact: true }).click();
    await page.getByLabel("Password", { exact: true }).fill("short");
    await page.getByRole("button", { name: "Create account", exact: true }).click();
    await page.getByText("Use at least 8 characters for your password.", { exact: true }).waitFor();
    check("Sign up validates its password before calling the service", !(await snapshot()).account.signed_in);
    await page.getByLabel("Password", { exact: true }).fill("fixture-password");
    await page.evaluate(() => {
      const preview = window.__SYNCHROME_PREVIEW__, original = preview.invoke.bind(preview);
      preview.invoke = async (command, args) => {
        if (command === "sign_up") {
          preview.invoke = original;
          return { ...preview.snapshot().account, error: "Account created. Confirm the e-mail we sent you, then sign in." };
        }
        return original(command, args);
      };
    });
    await page.getByRole("button", { name: "Create account", exact: true }).click();
    await page.getByRole("heading", { name: "Check your inbox", exact: true }).waitFor();
    check("Account confirmation is a success panel", await page.locator(".confirmation[role=status]").count() === 1);
    await page.getByRole("button", { name: "Back to sign in", exact: true }).click();
    await page.getByLabel("Password", { exact: true }).fill("fixture-password");
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await button("Choose a browser").waitFor();
    check("First entry explains the three-part profile relationship", (await modal().innerText()).includes("Other Macs"));
    await button("Explore workspace").click();
    await nav("settings");
    await page.getByRole("button", { name: "Sign out", exact: true }).click();
    check("Sign out is confirmed and explains preservation", (await modal().innerText()).includes("browser data and shared profiles are kept"));
    await button("Sign out").click();
    await page.locator("form.login").waitFor();
    check("Sign out leaves the authentication view", !(await snapshot()).account.signed_in);
  });

  await run("Onboarding and import", async () => {
    await scenario("onboarding");
    await button("Choose a browser").click();
    check("Browser access guidance explains Full Disk Access", (await modal().innerText()).includes("Full Disk Access"));
    await chooseLocal("Helium — Personal");
    await modal().getByLabel("Profile name", { exact: true }).fill("Imported via UI");
    await button("Import profile").click();
    await page.getByRole("heading", { name: "Finish connecting your browser", exact: true }).waitFor();
    check("Import creates one central profile and one pending connection", (await snapshot()).profiles.length === 1 && (await snapshot()).mappings[0].bookmarks === null);
    await button("Install connector").click();
    await wait(() => document.querySelector("dialog[open]")?.textContent.includes("Connector installed"));
    check("Companion setup provides the concrete next steps", (await modal().innerText()).includes("Load unpacked"));
    await page.evaluate(() => {
      const preview = window.__SYNCHROME_PREVIEW__;
      preview.confirmMapping(preview.snapshot().mappings[0].id);
    });
    await button("Check connection").click();
    await wait(() => document.querySelector("dialog[open]")?.textContent.includes("Waiting for first sync"));
    check("Companion confirmation does not masquerade as import completion", (await modal().getByRole("heading").first().innerText()) === "Finish connecting your browser");
    await button("Sync now").click();
    await page.getByRole("heading", { name: "Your first sync is complete.", exact: true }).waitFor();
    check("Completion follows an observed successful first upload", (await snapshot()).mappings[0].bookmarks.last_upload_at > 0);
    await button("View profile").click();
  });

  await run("Workspace and connection changes", async () => {
    await scenario("workspace");
    check("Overview shows central profiles and distinct Mac counts", await page.locator(".profile-overview-row").count() === 3 && (await page.getByRole("button", { name: "Open profile Work", exact: true }).innerText()).includes("2 Macs"));
    check("Mixed attention acknowledges setup or recovery", (await page.locator(".attention-strip").innerText()).includes("Other connections also need setup or recovery"));
    await page.locator(".attention-strip").getByRole("button", { name: "Review conflicts", exact: true }).click();
    check("Attention action matches the described conflict task", await page.getByTestId("nav-activity").getAttribute("aria-current") === "page");
    await profile("Work");
    check("Connections are grouped by Mac", await page.locator(".device-group").count() === 2);
    check("Remote connections avoid live-state claims", (await page.locator(".remote-note").innerText()).includes("does not show its live status"));
    check("Unsupported second local connection is prevented", await page.getByRole("button", { name: "Connect browser", exact: true }).isDisabled());
    await page.getByRole("button", { name: "Change browser", exact: true }).click();
    await chooseLocal("Helium — Reading");
    await page.evaluate(() => window.__SYNCHROME_PREVIEW__.failNext("map_local_profile", "network connection failed"));
    await button("Connect browser").click();
    await modal().locator(".form-error").waitFor();
    check("Failed replacement explains recovery after disconnection", !(await snapshot()).mappings.some(m => m.is_this_device && m.profile_name === "Work"));
    await button("Connect browser").click();
    await page.getByRole("heading", { name: "Finish connecting your browser", exact: true }).waitFor();
    check("Retry connects the new browser without repeating the old disconnect", (await snapshot()).mappings.some(m => m.is_this_device && m.profile_name === "Work" && m.browser_kind === "helium"));
    await button("Continue later").click();
    await page.getByRole("button", { name: "Disconnect", exact: true }).first().click();
    check("Disconnect confirmation keeps local browser data explicit", (await modal().innerText()).includes("local profile, bookmarks and files are kept"));
    await button("Disconnect browser").click();
    await wait(() => !document.querySelector("dialog[open]"));
    check("Disconnect retains the central profile", (await snapshot()).profiles.some(p => p.name === "Work"));
  });

  await run("Manual creation and protected dialogs", async () => {
    await scenario("workspace");
    await page.getByRole("button", { name: "New profile", exact: true }).click();
    await modal().getByRole("button", { name: /^Start fresh/ }).click();
    await modal().getByLabel("Profile name", { exact: true }).fill("Personal");
    await button("Create profile").click();
    await modal().locator(".form-error").waitFor();
    check("Duplicate names are validated inline", (await snapshot()).profiles.length === 3);
    await modal().getByLabel("Profile name", { exact: true }).fill("A new space");
    await page.evaluate(() => {
      const preview = window.__SYNCHROME_PREVIEW__, original = preview.invoke.bind(preview);
      preview.invoke = async (command, args) => {
        if (command === "create_profile") {
          preview.invoke = original;
          await new Promise(resolve => setTimeout(resolve, 1000));
        }
        return original(command, args);
      };
    });
    await button("Create profile").click();
    await page.keyboard.press("Escape");
    check("An in-flight mutation protects its dialog and submit action", await modal().count() === 1 && await modal().getByRole("button", { name: "Creating…", exact: true }).isDisabled());
    await wait(() => !document.querySelector("dialog[open]"));
    check("Manual creation leaves browsers unconnected", (await snapshot()).profiles.length === 4 && (await snapshot()).mappings.length === 5);
    await page.getByRole("button", { name: "New profile", exact: true }).click();
    await page.keyboard.press("Escape");
    await wait(() => !document.querySelector("dialog[open]"));
    check("Escape restores the invoking control", await page.getByRole("button", { name: "New profile", exact: true }).evaluate(el => el === document.activeElement));
  });

  await run("Deletion and conflicts", async () => {
    await scenario("workspace");
    const browserData = JSON.stringify((await snapshot()).browserData);
    await profile("Personal");
    await page.getByRole("button", { name: "Delete Personal", exact: true }).click();
    check("Delete shows separate cloud removal and local retention", (await modal().innerText()).includes("Your browser data stays") && await button("Delete profile").isDisabled());
    await page.locator("#delete-name").fill("wrong name");
    check("Delete rejects a mismatched name", await button("Delete profile").isDisabled());
    await page.locator("#delete-name").fill("Personal");
    await page.evaluate(() => window.__SYNCHROME_PREVIEW__.failNext("unmap_local_profile", "network connection failed"));
    await button("Delete profile").click();
    await modal().locator(".form-error").waitFor();
    check("Partial deletion failure leaves the cloud profile retryable", (await snapshot()).profiles.some(p => p.name === "Personal"));
    await button("Delete profile").click();
    await wait(() => !document.querySelector("dialog[open]"));
    const deleted = await snapshot();
    check("Retry safely removes cloud profile and all its relationships", !deleted.profiles.some(p => p.name === "Personal") && !deleted.mappings.some(m => m.profile_name === "Personal"));
    check("Profile deletion preserves every local browser fixture", JSON.stringify(deleted.browserData) === browserData);
    await nav("activity");
    await page.locator(".conflict").waitFor();
    await page.getByRole("button", { name: "Keep this device's version", exact: true }).click();
    await wait(() => window.__SYNCHROME_PREVIEW__.snapshot().mappings.find(m => m.is_this_device && m.profile_name === "Work").bookmarks.download.conflicts === 0);
    check("Conflict decision resolves the fixture conflict", Object.values((await snapshot()).conflicts).flat().some(c => c.resolution === "keep_local"));
  });

  await run("Appearance, errors and desktop width", async () => {
    await scenario("offline");
    check("Offline preserves local changes in its explanation", (await page.locator("main").innerText()).includes("Your local changes are kept"));
    await scenario("empty");
    await button("Explore workspace").click();
    check("Empty workspace offers meaningful creation choices", (await page.locator(".empty-state").innerText()).includes("Start fresh"));
    await scenario("workspace");
    await nav("settings");
    await page.getByRole("button", { name: "Dark", exact: true }).click();
    await wait(() => document.documentElement.dataset.theme === "dark");
    check("Dark theme is applied with native control color scheme", await page.evaluate(() => document.documentElement.style.colorScheme === "dark"));
    await page.getByRole("button", { name: "Light", exact: true }).click();
    await wait(() => document.documentElement.dataset.theme === "light");
    check("Appearance persists only the local presentation preference", await page.evaluate(() => localStorage.getItem("synchrome.appearance") === "light"));
    await page.setViewportSize({ width: 720, height: 488 });
    for (const route of ["overview", "profiles", "devices", "activity", "settings"]) {
      await nav(route);
      check(`${route} has no horizontal overflow at 720 px`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
    }
    check("Preview banner does not clip account access", await page.locator(".account-button").evaluate(el => el.getBoundingClientRect().bottom <= innerHeight + 1));
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.getByRole("button", { name: "New profile", exact: true }).count().then(async count => {
      if (!count) await nav("profiles");
    });
    await page.getByRole("button", { name: "New profile", exact: true }).click();
    check("Reduced motion disables dialog animation", await modal().evaluate(el => getComputedStyle(el).animationDuration === "0.01ms" || parseFloat(getComputedStyle(el).animationDuration) <= 0.001));
    await page.keyboard.press("Escape");
    await page.emulateMedia({ reducedMotion: "no-preference" });
  });
  check("No uncaught browser errors occurred", errors.length === 0);
  await scenario("workspace");
  await page.setViewportSize({ width: 1100, height: 780 });
  const report = {
    generatedAt: new Date().toISOString(), runtime: "Playwright / Chromium",
    scope: "Complementary real frontend checks with explicit token-free DEV fixtures; native acceptance is recorded separately",
    checks, passed: checks.filter(c => c.passed).length, failed: checks.filter(c => !c.passed).length, errors,
  };
  return report;
}
