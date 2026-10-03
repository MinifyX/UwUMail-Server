-- A label of the person's that became a base label because of its name ("Rechnung", "Newsletter"
-- …) gets the base label's definition; the description the person had written is kept here instead
-- of being lost (security review 0.22 LABELS22-L2). It is shown over JMAP and given to the model as
-- an extra hint.
ALTER TABLE assist_labels ADD COLUMN previous_description TEXT;
