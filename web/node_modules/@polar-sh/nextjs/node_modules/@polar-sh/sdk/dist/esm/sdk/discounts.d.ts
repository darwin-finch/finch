import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { Discount } from "../models/components/discount.js";
import { DiscountCreate } from "../models/components/discountcreate.js";
import { DiscountsDeleteRequest } from "../models/operations/discountsdelete.js";
import { DiscountsGetRequest } from "../models/operations/discountsget.js";
import { DiscountsListRequest, DiscountsListResponse } from "../models/operations/discountslist.js";
import { DiscountsUpdateRequest } from "../models/operations/discountsupdate.js";
import { PageIterator } from "../types/operations.js";
export declare class Discounts extends ClientSDK {
    /**
     * List Discounts
     *
     * @remarks
     * List discounts.
     *
     * **Scopes**: `discounts:read` `discounts:write`
     */
    list(request: DiscountsListRequest, options?: RequestOptions): Promise<PageIterator<DiscountsListResponse, {
        page: number;
    }>>;
    /**
     * Create Discount
     *
     * @remarks
     * Create a discount.
     *
     * **Scopes**: `discounts:write`
     */
    create(request: DiscountCreate, options?: RequestOptions): Promise<Discount>;
    /**
     * Get Discount
     *
     * @remarks
     * Get a discount by ID.
     *
     * **Scopes**: `discounts:read` `discounts:write`
     */
    get(request: DiscountsGetRequest, options?: RequestOptions): Promise<Discount>;
    /**
     * Update Discount
     *
     * @remarks
     * Update a discount.
     *
     * **Scopes**: `discounts:write`
     */
    update(request: DiscountsUpdateRequest, options?: RequestOptions): Promise<Discount>;
    /**
     * Delete Discount
     *
     * @remarks
     * Delete a discount.
     *
     * **Scopes**: `discounts:write`
     */
    delete(request: DiscountsDeleteRequest, options?: RequestOptions): Promise<void>;
}
//# sourceMappingURL=discounts.d.ts.map