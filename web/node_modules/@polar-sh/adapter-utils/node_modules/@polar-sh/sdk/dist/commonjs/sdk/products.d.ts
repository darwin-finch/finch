import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { Product } from "../models/components/product.js";
import { ProductCreate } from "../models/components/productcreate.js";
import { ProductsGetRequest } from "../models/operations/productsget.js";
import { ProductsListRequest, ProductsListResponse } from "../models/operations/productslist.js";
import { ProductsUpdateRequest } from "../models/operations/productsupdate.js";
import { ProductsUpdateBenefitsRequest } from "../models/operations/productsupdatebenefits.js";
import { PageIterator } from "../types/operations.js";
export declare class Products extends ClientSDK {
    /**
     * List Products
     *
     * @remarks
     * List products.
     *
     * **Scopes**: `products:read` `products:write`
     */
    list(request: ProductsListRequest, options?: RequestOptions): Promise<PageIterator<ProductsListResponse, {
        page: number;
    }>>;
    /**
     * Create Product
     *
     * @remarks
     * Create a product.
     *
     * **Scopes**: `products:write`
     */
    create(request: ProductCreate, options?: RequestOptions): Promise<Product>;
    /**
     * Get Product
     *
     * @remarks
     * Get a product by ID.
     *
     * **Scopes**: `products:read` `products:write`
     */
    get(request: ProductsGetRequest, options?: RequestOptions): Promise<Product>;
    /**
     * Update Product
     *
     * @remarks
     * Update a product.
     *
     * **Scopes**: `products:write`
     */
    update(request: ProductsUpdateRequest, options?: RequestOptions): Promise<Product>;
    /**
     * Update Product Benefits
     *
     * @remarks
     * Update benefits granted by a product.
     *
     * **Scopes**: `products:write`
     */
    updateBenefits(request: ProductsUpdateBenefitsRequest, options?: RequestOptions): Promise<Product>;
}
//# sourceMappingURL=products.d.ts.map