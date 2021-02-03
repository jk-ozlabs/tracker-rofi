Small utility to query the Gnome tracker database from rofi

# Integration

You'll need a version of rofi that populates the `ROFI_INFO` environment
variable - 1.6.0 or later.

Run rofi with a mode for the tracker executable:

     rofi -modi tracker:/path/to/tracker-rofi

For a suitable i3/swaywm configuration, this shows a drun menu by default,
switchable to a tracker search (with ctrl+tab):

    bindysym $mod+d exec rofi -modi drun#tracker:/path/to/tracker-rofi -show drun

# TODO

 * Pagination for >15 results
 * Better handling for no matches
