#include <gtk/gtk.h>

static void copy_file(GSimpleAction *action, GVariant *parameter, gpointer data)
{
    (void)parameter;
    GtkWidget *window = data;
    GdkContentProvider *file = g_object_get_data(G_OBJECT(action), "file");
    gboolean copied = gdk_clipboard_set_content(gtk_widget_get_clipboard(window), file);
    gtk_window_set_title(GTK_WINDOW(window), copied ? "Archivo copiado" : "Error al copiar");
}

static void activate(GtkApplication *app, gpointer data)
{
    GdkContentProvider *file = data;
    GtkWidget *window = gtk_application_window_new(app);
    gtk_window_set_title(GTK_WINDOW(window), "Transferencia de archivos");
    gtk_window_set_default_size(GTK_WINDOW(window), 360, 140);

    GtkWidget *box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 12);
    gtk_widget_set_margin_start(box, 16);
    gtk_widget_set_margin_end(box, 16);
    gtk_widget_set_margin_top(box, 16);
    gtk_widget_set_margin_bottom(box, 16);
    gtk_window_set_child(GTK_WINDOW(window), box);

    GtkWidget *row = gtk_label_new("Arrastra este archivo a Dolphin");
    gtk_widget_set_vexpand(row, TRUE);
    gtk_box_append(GTK_BOX(box), row);
    GtkDragSource *drag = gtk_drag_source_new();
    gtk_drag_source_set_actions(drag, GDK_ACTION_COPY);
    gtk_drag_source_set_content(drag, file);
    gtk_widget_add_controller(row, GTK_EVENT_CONTROLLER(drag));

    GSimpleAction *copy = g_simple_action_new("copy-file", NULL);
    g_object_set_data(G_OBJECT(copy), "file", file);
    g_signal_connect(copy, "activate", G_CALLBACK(copy_file), window);
    g_action_map_add_action(G_ACTION_MAP(window), G_ACTION(copy));
    g_object_unref(copy);
    const char *accelerators[] = {"<Control>c", NULL};
    gtk_application_set_accels_for_action(app, "win.copy-file", accelerators);

    GtkWidget *button = gtk_button_new_with_label("Copiar archivo");
    gtk_actionable_set_action_name(GTK_ACTIONABLE(button), "win.copy-file");
    gtk_box_append(GTK_BOX(box), button);
    gtk_window_present(GTK_WINDOW(window));
}

int main(int argc, char **argv)
{
    if (argc != 2 || !g_file_test(argv[1], G_FILE_TEST_IS_REGULAR)) {
        g_printerr("Uso: %s ARCHIVO_LOCAL\n", argv[0]);
        return 2;
    }
    GFile *file = g_file_new_for_path(argv[1]);
    char *uri = g_file_get_uri(file);
    char *uri_list = g_strconcat(uri, "\r\n", NULL);
    GBytes *bytes = g_bytes_new_take(uri_list, strlen(uri_list));
    GdkContentProvider *provider = gdk_content_provider_new_for_bytes("text/uri-list", bytes);
    g_bytes_unref(bytes);
    g_free(uri);
    g_object_unref(file);

    GtkApplication *app = gtk_application_new("dev.vscp.FileHandoff", G_APPLICATION_NON_UNIQUE);
    g_signal_connect(app, "activate", G_CALLBACK(activate), provider);
    int result = g_application_run(G_APPLICATION(app), 1, argv);
    g_object_unref(app);
    g_object_unref(provider);
    return result;
}
