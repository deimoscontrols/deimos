# Contact

Questions, comments, or suggestions? Use this form to send us a note!

<form id="contact-form" action="https://formspree.io/f/xykndlgy" method="POST" class="contact-form">
  <div class="contact-form__row">
    <label for="contact-name">Name</label>
    <input id="contact-name" name="name" type="text" autocomplete="name" required>
  </div>

  <div class="contact-form__row">
    <label for="contact-email">Email</label>
    <input id="contact-email" name="email" type="email" autocomplete="email" required>
  </div>

  <div class="contact-form__row">
    <label for="contact-organization">Organization <span class="contact-form__optional">(optional)</span></label>
    <input id="contact-organization" name="organization" type="text" autocomplete="organization">
  </div>

  <div class="contact-form__checkbox">
    <label for="contact-optin">
      <input id="contact-optin" name="mailing_list_opt_in" type="checkbox" value="yes">
      Add me to the mailing list for product updates!
    </label>
  </div>

  <div class="contact-form__row">
    <label for="contact-message">Message</label>
    <textarea id="contact-message" name="message" rows="8" required></textarea>
  </div>

  <input type="hidden" name="_subject" value="Website contact form submission">

  <button type="submit" class="md-button md-button--primary">Send Message</button>

  <p class="contact-form__privacy">
    We use your information to respond to your message.<br>
    Submissions are processed by Formspree.<br>
    If you opt-in to the mailing list, we will send your information to Mailchimp.<br>
    You can unsubscribe from emails using the link in the email footer. <br>
    Please do not send confidential information through this form.
  </p>
</form>

<form
  id="contact-mailchimp-form"
  action="https://deimoscontrols.us13.list-manage.com/subscribe/post?u=d3f613c56803a4b84867251d4&amp;id=62e300351a&amp;f_id=005103e9f0"
  method="post"
  target="contact-mailchimp-target"
  hidden
>
  <input id="contact-mailchimp-email" type="email" name="EMAIL" value="">
  <input id="contact-mailchimp-name" type="text" name="FNAME" value="">
  <input id="contact-mailchimp-organization" type="text" name="COMPANY" value="">
  <input type="text" name="b_d3f613c56803a4b84867251d4_62e300351a" tabindex="-1" value="">
</form>

<iframe
  id="contact-mailchimp-target"
  name="contact-mailchimp-target"
  title="Mailing list signup target"
  hidden
></iframe>
