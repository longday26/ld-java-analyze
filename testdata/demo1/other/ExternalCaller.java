package demo;

/** Lives outside `src/`, so it disappears when the reference filter is set to `src`. */
public class ExternalCaller {
    private UserService service;

    public User load(int id) {
        return service.findById(id);
    }
}
