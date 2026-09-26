from dotenv import load_dotenv

load_dotenv()

from providers import get_provider


def main():
    provider = get_provider()

    print(f"Provider loaded: {provider.__class__.__name__}")

    prompt = """
    You are analyzing a Git commit for an engineering history tour.

    Explain in 2 short sentences why an important bug-fix commit
    might be significant to a new software engineer.
    """

    result = provider.generate(prompt)

    print("\nModel response:")
    print(result)


if __name__ == "__main__":
    main()