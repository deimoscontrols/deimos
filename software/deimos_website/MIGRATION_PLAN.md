# Website Migration Plan

This moves `deimoscontrols.com` from the old `deimos-website` repository to the
monorepo-hosted site in `software/deimos_website`.

## Deployment Setup

1. Merge the website deployment workflow into `deimos/main`.

2. In the `deimos` repository, open `Settings -> Pages`.

3. Under `Build and deployment`, set `Source` to `GitHub Actions`.

4. Run the workflow manually once:

   - `Actions -> deploy-website -> Run workflow`

5. Confirm the workflow publishes successfully at the temporary GitHub Pages URL
   before moving the custom domain.

## Custom Domain Handoff

1. In the old `deimos-website` repository, open `Settings -> Pages`.

2. Remove the custom domain:

   - `deimoscontrols.com`

3. Disable Pages on the old repository, or leave it enabled without a custom
   domain.

4. In the new `deimos` repository, open `Settings -> Pages`.

5. Under `Custom domain`, add:

   - `deimoscontrols.com`

6. Save the setting.

7. Enable `Enforce HTTPS` once GitHub allows it. Certificate provisioning can
   take some time after the custom domain is added.

## DNS

DNS probably does not need to change if the old site was already hosted under
the `deimoscontrols` GitHub organization. DNS points to GitHub Pages, while the
repository Pages settings decide which repository serves the domain.

The apex domain should point to GitHub Pages:

```text
185.199.108.153
185.199.109.153
185.199.110.153
185.199.111.153
```

The `www` subdomain, if used, should be a `CNAME` pointing to:

```text
deimoscontrols.github.io
```

Do not point `www` at a repository-specific URL.

## Troubleshooting

If GitHub reports that the custom domain is already taken:

1. Confirm `deimoscontrols.com` was removed from the old `deimos-website`
   repository's Pages settings.

2. Verify the domain at the GitHub organization/account level if GitHub asks for
   it.

3. Wait for GitHub's Pages/custom-domain state to settle, then retry.

4. If the domain remains stuck, contact GitHub Support to release the domain
   mapping.

If HTTPS is unavailable after the move:

1. Confirm the custom domain is saved in the new repository's Pages settings.

2. Confirm DNS still points to GitHub Pages.

3. Wait for certificate provisioning.

4. If needed, remove and re-add the custom domain in the new repository to
   restart HTTPS provisioning.

## Notes

With GitHub Actions based Pages deployment, the repository Pages setting is the
source of truth for the custom domain. A `CNAME` file in the generated site is
not required by GitHub Actions Pages deployment, though keeping
`docs/CNAME` is useful for local/static portability.

Relevant GitHub documentation:

- <https://docs.github.com/en/pages/getting-started-with-github-pages/configuring-a-publishing-source-for-your-github-pages-site>
- <https://docs.github.com/en/pages/getting-started-with-github-pages/using-custom-workflows-with-github-pages>
- <https://docs.github.com/en/pages/configuring-a-custom-domain-for-your-github-pages-site/managing-a-custom-domain-for-your-github-pages-site>
