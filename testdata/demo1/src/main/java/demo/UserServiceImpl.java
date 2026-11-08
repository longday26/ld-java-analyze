package demo;

public class UserServiceImpl implements UserService {
    private User current;

    @Override
    public User findById(int id) {
        return current;
    }

    @Override
    public String hello(String who) {
        return "hello " + who;
    }
}
