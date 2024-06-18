#include <linux/pci.h>

void rust_helper_pci_set_drvdata(struct pci_dev *pdev, void *data)
{
	pci_set_drvdata(pdev, data);
}

void *rust_helper_pci_get_drvdata(struct pci_dev *pdev)
{
	return pci_get_drvdata(pdev);
}

u64 rust_helper_pci_resource_len(struct pci_dev *pdev, int barnr)
{
	return pci_resource_len(pdev, barnr);
}
