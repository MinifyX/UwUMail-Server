-- A shipment event's text is its mail's subject: when the mail goes, so does the text
-- (status and time stay).
CREATE TRIGGER shipment_events_forget_mail BEFORE DELETE ON emails
BEGIN
    UPDATE shipment_events SET text = NULL WHERE email_id = OLD.id;
END;
