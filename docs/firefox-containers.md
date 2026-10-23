# Opening links in a Firefox container

If you use Firefox with containers (for example the Mozilla add-on
[Multi-Account Containers](https://addons.mozilla.org/firefox/addon/multi-account-containers/)), you may want the links
of one source, say work Azure DevOps, to open in your "Work" container. There are two ways. Most people only need the
first.

## Way 1: a site rule in Firefox (no change in DeskWatch)

In Multi-Account Containers, open `dev.azure.com` once, click the add-on icon and choose **Always open this site in**
your container. From then on every link to that site opens in it, from DeskWatch or anywhere else. It needs no extra
add-on and shows no confirmation.

## Way 2: a container per source in DeskWatch

Use this when a site rule is not enough, for example when two sources share one address.

1. In Firefox, install the add-on
   [Open external links in a container](https://addons.mozilla.org/firefox/addon/open-url-in-container/). It is a
   separate add-on by another author, not part of Multi-Account Containers. It handles the `ext+container:` links
   DeskWatch writes, and it uses the same Firefox containers.
2. In Setup > Sources, open the source and fill in **Firefox container for links**, for example `Work`. Or set
   `container = "Work"` in the source block of `bridge.toml`:

   ```toml
   [[source.azure_devops]]
   name = "work"
   organization = "your-org"
   projects = ["project-a"]
   token_file = "azdo-work"
   container = "Work"
   ```

Leave the field empty and nothing changes: the links stay normal links. It is off by default, so other users, other
browsers and other sources are not affected.

### What to know

- **Firefox only.** The page checks the browser. In any other browser the field has no effect and links stay plain.
- **Per source type.** A link on the dashboard knows the type of its source (`azure_devops`, `github`, ...), not the
  name of the block. With two blocks of one type, the first block that names a container decides for all links of
  that type.
- **New tab.** Like other links, these open in a new tab, so the dashboard stays open. The add-on then moves it
  to the container. Without this, the add-on took over the dashboard tab.
- **Confirmation.** The add-on asks "open this link?" for links without a signature, and Firefox can show its own
  warning for external protocols (since Firefox 121). If that gets in the way, the add-on's own issue tracker
  describes a Firefox setting (`network.protocol-handler.external.ext+container` in `about:config`). Way 1 avoids this.
- **Not verified on a real Firefox yet.** The link format follows the add-on's documentation and is covered by tests of
  the generated links; clicking one with the add-on installed has not been tried on a real setup.
