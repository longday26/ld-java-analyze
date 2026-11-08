package demo;

public interface UserMapper {
    User selectById(int id);

    int insert(User user);
}
