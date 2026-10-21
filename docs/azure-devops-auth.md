# Azure DevOps without a personal access token: research and recommendation

The `azure_devops` source signs in with a personal access token (PAT), see [azure-devops.md](azure-devops.md).
A PAT acts as one person, expires, and some organisations forbid it. This page looks at the alternatives and says
which one DeskWatch should build. Nothing here is built yet; it is the basis for a follow-up change.

Checked on 2026-10-21 against Microsoft's documentation (links at the end).

## What the bridge needs

Read-only `GET` calls from a server at home or in Docker, every few seconds, for months, with nobody around to
click through a sign-in page. So the credential must be usable headless and must renew itself.

## The options

| Option | Acts as | Headless | Verdict |
|---|---|---|---|
| **PAT** (today) | A person | Yes | Works everywhere. Microsoft calls it the fallback for tools that cannot do Entra. Org-scoped PATs are fine; global PATs stop working on 1 December 2026 (DeskWatch already asks for an organisation-scoped token). Admins may forbid PATs or cap their lifetime. |
| **Entra service principal** (client secret or certificate) | An app, not a person | Yes | **Recommended as the second option.** |
| **Managed identity** | An app, not a person | Yes, but only on Azure | Only for a bridge that runs on an Azure VM or container. Not for a homelab or a Pi. |
| **Entra OAuth, delegated** (a person signs in, the app keeps a refresh token) | A person | Needs one interactive sign-in, then refresh | Not recommended: a person's session in a server, Conditional Access (MFA, device rules) can break the refresh, and it needs a sign-in page flow in the bridge. |
| **Azure DevOps OAuth apps** (the older, own OAuth) | A person | n/a | Dead end: no new app registrations since 23 April 2025, end of life in 2026. |
| **Workload identity federation** (a service principal with no secret at all) | An app | Yes, if the host can issue a trusted OIDC token | Nice for GitHub Actions or Kubernetes. A homelab has no such issuer, so it only helps later. |
| Service hooks (Azure DevOps pushes to us) | n/a | n/a | Needs a port open to the internet. Against the design of this project. |

## Recommendation: service principal with a client secret (certificate later)

How it works, with the facts that matter:

1. An Entra admin registers an app in the tenant that backs the Azure DevOps organisation and makes a client secret
   for it (or uploads a certificate).
2. An Azure DevOps organisation admin adds that service principal as a user (Organization settings, Users, Add users;
   use the Object ID from **Enterprise applications**, not from App registrations) with the **Basic** access level on
   the projects to watch, and gives it Reader-style rights. Entra "application permissions" play no role: Azure DevOps
   authorises through access level, groups and resource permissions.
3. The bridge asks Entra for a token with the client-credentials flow:
   `POST https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token` with
   `scope=499b84ac-1321-427f-aa17-267ca6975798/.default` (the Azure DevOps resource), and sends it as
   `Authorization: Bearer <token>` instead of the PAT's Basic header. Every Azure DevOps REST API that takes a PAT
   takes this token too, so the rest of the source is unchanged (same URLs, same API version 7.1).
4. Tokens are short lived. Microsoft says to treat them as opaque and not rely on a fixed lifetime, so the bridge keeps
   the token in memory, renews it a few minutes before `expires_in`, and fetches a new one when a call returns 401.

What this gives over a PAT:

- It is not tied to a person: nothing breaks when someone leaves, and the audit trail shows an app.
- Access is only what the identity is granted, and removing it from the organisation cuts it off at once.
- The token lives about an hour, so a copy of it found in a log or a backup is nearly worthless. Only the client
  secret is long lived, and it can only be used against Entra, not directly against Azure DevOps.
- No PAT policy of the organisation applies (it keeps working if PAT creation is switched off).

What it costs:

- **A licence.** Every application identity takes a paid access level in each organisation it is added to
  (multi-organisation billing and Visual Studio subscription benefits do not apply). Check the current price of one
  Basic user; for a private organisation that is a real cost, for a company it is usually small.
- **Two admins.** An Entra admin (app and secret) and an Azure DevOps organisation admin (add the identity). A PAT
  needs neither. At an employer this is a conversation, but it is also the route security teams ask for.
- **A secret that still expires.** A client secret has an end date (Entra allows up to two years), so it is not less
  work than a PAT, only less risky. A certificate or workload identity federation avoids the secret.
- The organisation must be connected to an Entra tenant. An organisation tied to personal Microsoft accounts cannot
  use this.
- An application identity cannot do what a person does in the web portal. For DeskWatch (read builds, pull requests,
  agent pools) that does not matter.

Managed identity is the same flow without any secret, but Microsoft only offers it to workloads hosted on Azure
(there the token comes from the local metadata endpoint, `169.254.169.254`). It is cheap to add as a third `auth` mode
for people who run the bridge on an Azure VM, and useless at home.

## How it would fit in DeskWatch

Config (the PAT stays the default, nothing changes for existing installs):

```toml
[[source.azure_devops]]
name = "work"
organization = "your-org"
projects = ["project-a"]
auth = "entra"                       # default "pat"
tenant_id = "00000000-0000-0000-0000-000000000000"
client_id = "00000000-0000-0000-0000-000000000000"
client_secret_file = "azdo-work"     # a credential, like token_file today; no secret in the config
```

Work in `bridge/src/azure_devops/`:

- A small token provider next to `api.rs`: PAT is a fixed header, Entra fetches and caches a bearer token and renews
  it early. The rest of the client asks the provider for the header on each request.
- `auth_failed` handling reused: a 401 or the sign-in redirect first forces one token renewal, then reports
  "token refused" (health badge) as now.
- Setup page: an "Authentication" choice in the Azure DevOps form (Token or Entra app) and a **Test connection** that
  says which step failed (Entra refused the secret, or Azure DevOps does not know the identity).
- Docs: a section in azure-devops.md with the admin steps, an "ask your admin" paragraph, and a note that real
  tenant, client and organisation names never go in the repo.
- Tests against a mock Entra endpoint and the existing mock Azure DevOps; no real tenant is needed to build it, but a
  real run by someone with an organisation (checklist in [azure-devops-verify.md](azure-devops-verify.md)) is what
  proves it.

## Overlap with the multi-user login

Signing people in to DeskWatch with Microsoft Entra (OAuth login, see [auth.md](auth.md)) is a separate thing from the
bridge reading Azure DevOps. They can use the same Entra tenant but different app registrations: one for people
signing in to the dashboard, one for the service principal that reads Azure DevOps. Keep them apart, so a leaked
dashboard secret cannot read pipelines and the reverse.

## Decision for the owner

Build the optional `auth = "entra"` mode (service principal with client secret, certificate later)? If yes, the
change is small and isolated to the Azure DevOps source and its Setup form, and the PAT stays the default.

## Sources

- Microsoft Learn, [Service principals and managed identities in Azure DevOps](https://learn.microsoft.com/en-us/azure/devops/integrate/get-started/authentication/service-principal-managed-identity?view=azure-devops)
- Microsoft Learn, [Microsoft Entra OAuth apps for Azure DevOps](https://learn.microsoft.com/en-us/azure/devops/integrate/get-started/authentication/entra-oauth?view=azure-devops)
- Microsoft Learn, [Use personal access tokens](https://learn.microsoft.com/en-us/azure/devops/organizations/accounts/use-personal-access-tokens-to-authenticate?view=azure-devops)
  (PATs only when nothing better exists; global PATs stop working on 1 December 2026)
- Azure DevOps blog, [No new Azure DevOps OAuth apps](https://devblogs.microsoft.com/devops/no-new-azure-devops-oauth-apps-beginning-february-2025/)
