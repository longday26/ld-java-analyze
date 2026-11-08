package demo;

public class UserController {
    private UserService service;
    private UserMapper mapper;

    public String show(int id) {
        User user = service.findById(id);
        mapper.selectById(id);
        return user.getName() + service.hello(user.getName());
    }
}
