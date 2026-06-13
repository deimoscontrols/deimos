document.addEventListener("DOMContentLoaded", () => {
  const contactForm = document.getElementById("contact-form");
  const mailchimpForm = document.getElementById("contact-mailchimp-form");
  const optIn = document.getElementById("contact-optin");

  if (!contactForm || !mailchimpForm || !optIn) {
    return;
  }

  contactForm.addEventListener("submit", () => {
    if (!optIn.checked) {
      return;
    }

    const name = contactForm.elements.namedItem("name");
    const email = contactForm.elements.namedItem("email");
    const organization = contactForm.elements.namedItem("organization");
    const mailchimpName = document.getElementById("contact-mailchimp-name");
    const mailchimpEmail = document.getElementById("contact-mailchimp-email");
    const mailchimpOrganization = document.getElementById("contact-mailchimp-organization");

    if (
      !(name instanceof HTMLInputElement) ||
      !(email instanceof HTMLInputElement) ||
      !(organization instanceof HTMLInputElement) ||
      !(mailchimpName instanceof HTMLInputElement) ||
      !(mailchimpEmail instanceof HTMLInputElement) ||
      !(mailchimpOrganization instanceof HTMLInputElement)
    ) {
      return;
    }

    mailchimpName.value = name.value.trim();
    mailchimpEmail.value = email.value.trim();
    mailchimpOrganization.value = organization.value.trim();
    mailchimpForm.submit();
  });
});
