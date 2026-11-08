package demo;

import lombok.Data;

@Data
public class User {
    private String name;
    private boolean active;

    public String greet(String prefix) {
        return prefix + name;
    }
}
